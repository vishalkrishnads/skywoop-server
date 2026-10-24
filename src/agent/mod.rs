pub mod tools;

use std::collections::HashMap;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::ai::{AiError, Effort, RenderedPrompt, TextRole};
use crate::domain::Platform;
use crate::domain::new_id;

#[derive(Clone)]
pub struct AgentCtx {
    pub project_id: Uuid,
    pub platform: Platform,
    pub plan_id: Option<Uuid>,
    pub db: sqlx::PgPool,
    pub state: Option<crate::state::AppState>,
}

#[async_trait]
pub trait AgentTool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn parameters_schema(&self) -> Value;
    async fn call(&self, ctx: &AgentCtx, args: Value) -> Result<Value, String>;
}

pub fn tool_defs(tools: &[&dyn AgentTool]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "name": t.name(),
                "description": t.description(),
                "parameters": t.parameters_schema(),
            })
        })
        .collect()
}

#[async_trait]
pub trait AgentModel: Send + Sync {
    async fn next_turn(
        &self,
        role: TextRole,
        purpose: &str,
        prompt: &RenderedPrompt,
        input: &[Value],
        tools: &[Value],
        effort: Effort,
        max_output_tokens: u32,
        output_schema: Option<&Value>,
    ) -> Result<AgentTurn, AiError>;
}

#[async_trait]
impl AgentModel for crate::ai::AiClient {
    async fn next_turn(
        &self,
        role: TextRole,
        purpose: &str,
        prompt: &RenderedPrompt,
        input: &[Value],
        tools: &[Value],
        effort: Effort,
        max_output_tokens: u32,
        output_schema: Option<&Value>,
    ) -> Result<AgentTurn, AiError> {
        self.responses_tool_loop_turn(
            role,
            purpose,
            prompt,
            input,
            tools,
            effort,
            max_output_tokens,
            output_schema,
        )
        .await
    }
}

#[derive(Debug)]
pub enum AgentTurn {
    FunctionCalls {
        calls: Vec<FunctionCall>,
        echo_items: Vec<Value>,
        usage: Option<Value>,
    },
    Final {
        text: String,
        usage: Option<Value>,
    },
}

#[derive(Debug, Clone)]
pub struct FunctionCall {
    pub call_id: String,
    pub name: String,
    pub arguments: String,
}

pub struct ToolLoopLimits {
    pub max_steps: usize,
    pub max_output_tokens: u32,
}

impl Default for ToolLoopLimits {
    fn default() -> Self {
        Self {
            max_steps: 40,
            max_output_tokens: 16000,
        }
    }
}

/// Wire-up once `AiClient::responses_tool_loop_turn` exists in `src/ai/client.rs`:
///
/// ```ignore
/// #[async_trait]
/// impl crate::agent::AgentModel for AiClient {
///     async fn next_turn(
///         &self,
///         role: TextRole,
///         purpose: &str,
///         prompt: &RenderedPrompt,
///         input: &[Value],
///         tools: &[Value],
///         effort: Effort,
///         max_output_tokens: u32,
///     ) -> Result<crate::agent::AgentTurn, AiError> {
///         self.responses_tool_loop_turn(
///             role, purpose, prompt, input, tools, effort, max_output_tokens,
///         )
///         .await
///     }
/// }
/// ```
pub async fn run_tool_loop<T: serde::de::DeserializeOwned + JsonSchema>(
    model: &dyn AgentModel,
    role: TextRole,
    purpose: &'static str,
    prompt: &RenderedPrompt,
    tools: &[&dyn AgentTool],
    ctx: &AgentCtx,
    effort: Effort,
    limits: ToolLoopLimits,
) -> Result<(T, Uuid), AiError> {
    let run_id = new_id();
    sqlx::query(
        r#"
        insert into agent_runs (id, plan_id, project_id, purpose, status, steps)
        values ($1, $2, $3, $4, 'running', '[]'::jsonb)
        "#,
    )
    .bind(run_id)
    .bind(ctx.plan_id)
    .bind(ctx.project_id)
    .bind(purpose)
    .execute(&ctx.db)
    .await
    .map_err(|e| AiError::Other(e.into()))?;

    let tool_map: HashMap<&str, &dyn AgentTool> = tools.iter().map(|t| (t.name(), *t)).collect();
    let tool_defs = tool_defs(tools);
    let output_schema = crate::ai::strict_schema::<T>();

    let mut input: Vec<Value> = vec![json!({
        "role": "user",
        "content": [{ "type": "input_text", "text": prompt.user }]
    })];
    let mut steps: Vec<Value> = Vec::new();
    let mut total_input_tokens: i32 = 0;
    let mut total_output_tokens: i32 = 0;

    for step_no in 0..limits.max_steps {
        let turn = model
            .next_turn(
                role,
                purpose,
                prompt,
                &input,
                &tool_defs,
                effort,
                limits.max_output_tokens,
                Some(&output_schema),
            )
            .await;

        let turn = match turn {
            Ok(t) => t,
            Err(e) => {
                finish_agent_run_failed(
                    &ctx.db,
                    run_id,
                    &steps,
                    total_input_tokens,
                    total_output_tokens,
                    e.to_string(),
                )
                .await;
                return Err(e);
            }
        };

        match turn {
            AgentTurn::FunctionCalls {
                calls,
                echo_items,
                usage,
            } => {
                if let Some(u) = &usage {
                    total_input_tokens += usage_i32(u, "input_tokens");
                    total_output_tokens += usage_i32(u, "output_tokens");
                }
                input.extend(echo_items);

                let mut step_record = json!({ "step": step_no, "calls": [] });
                let calls_arr = step_record["calls"].as_array_mut().unwrap();

                for call in &calls {
                    calls_arr.push(json!({
                        "call_id": call.call_id,
                        "name": call.name,
                        "arguments": call.arguments,
                    }));

                    let args: Value = serde_json::from_str(&call.arguments).unwrap_or(Value::Null);
                    let tool_result = match tool_map.get(call.name.as_str()) {
                        Some(tool) => match tool.call(ctx, args).await {
                            Ok(v) => v.to_string(),
                            Err(e) => format!(r#"{{"error":{}}}"#, json!(e)),
                        },
                        None => format!(
                            r#"{{"error":"unknown tool {}"}}"#,
                            call.name.replace('"', "\\\"")
                        ),
                    };

                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": call.call_id,
                        "output": tool_result,
                    }));
                }
                steps.push(step_record);
            }
            AgentTurn::Final { text, usage } => {
                if let Some(u) = &usage {
                    total_input_tokens += usage_i32(u, "input_tokens");
                    total_output_tokens += usage_i32(u, "output_tokens");
                }
                steps.push(json!({ "step": step_no, "final": true }));

                let parsed: Result<T, _> = serde_json::from_str(&text);
                match parsed {
                    Ok(result) => {
                        sqlx::query(
                            r#"
                            update agent_runs
                               set status = 'succeeded',
                                   steps = $2,
                                   input_tokens = nullif($3, 0),
                                   output_tokens = nullif($4, 0),
                                   finished_at = now()
                             where id = $1
                            "#,
                        )
                        .bind(run_id)
                        .bind(json!(steps))
                        .bind(total_input_tokens)
                        .bind(total_output_tokens)
                        .execute(&ctx.db)
                        .await
                        .map_err(|e| AiError::Other(e.into()))?;
                        return Ok((result, run_id));
                    }
                    Err(e) => {
                        let msg = format!("invalid final JSON: {e}");
                        finish_agent_run_failed(
                            &ctx.db,
                            run_id,
                            &steps,
                            total_input_tokens,
                            total_output_tokens,
                            msg.clone(),
                        )
                        .await;
                        return Err(AiError::InvalidOutput(msg));
                    }
                }
            }
        }
    }

    let msg = format!("tool loop exceeded max_steps ({})", limits.max_steps);
    finish_agent_run_failed(
        &ctx.db,
        run_id,
        &steps,
        total_input_tokens,
        total_output_tokens,
        msg.clone(),
    )
    .await;
    Err(AiError::Incomplete(msg))
}

async fn finish_agent_run_failed(
    db: &sqlx::PgPool,
    run_id: Uuid,
    steps: &[Value],
    in_tok: i32,
    out_tok: i32,
    err: String,
) {
    let _ = sqlx::query(
        r#"
        update agent_runs
           set status = 'failed',
               steps = $2,
               input_tokens = nullif($3, 0),
               output_tokens = nullif($4, 0),
               error = $5,
               finished_at = now()
         where id = $1
        "#,
    )
    .bind(run_id)
    .bind(json!(steps))
    .bind(in_tok)
    .bind(out_tok)
    .bind(err)
    .execute(db)
    .await;
}

fn usage_i32(usage: &Value, key: &str) -> i32 {
    usage
        .get(key)
        .and_then(|v| v.as_i64())
        .map(|n| n as i32)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use schemars::JsonSchema;
    use serde::Deserialize;
    use std::sync::Mutex;

    #[derive(Debug, Deserialize, JsonSchema)]
    struct TinyOut {
        answer: String,
    }

    struct ScriptedModel {
        turns: Mutex<Vec<AgentTurn>>,
    }

    #[async_trait]
    impl AgentModel for ScriptedModel {
        async fn next_turn(
            &self,
            _role: TextRole,
            _purpose: &str,
            _prompt: &RenderedPrompt,
            _input: &[Value],
            _tools: &[Value],
            _effort: Effort,
            _max_output_tokens: u32,
            _output_schema: Option<&Value>,
        ) -> Result<AgentTurn, AiError> {
            let mut g = self.turns.lock().unwrap();
            if g.is_empty() {
                return Err(AiError::NoOutput);
            }
            // pop from front
            Ok(g.remove(0))
        }
    }

    struct EchoTool;

    #[async_trait]
    impl AgentTool for EchoTool {
        fn name(&self) -> &'static str {
            "echo"
        }
        fn description(&self) -> &'static str {
            "echo args"
        }
        fn parameters_schema(&self) -> Value {
            json!({
                "type": "object",
                "properties": { "x": { "type": "string" } },
                "required": ["x"],
                "additionalProperties": false
            })
        }
        async fn call(&self, _ctx: &AgentCtx, args: Value) -> Result<Value, String> {
            Ok(args)
        }
    }

    #[tokio::test]
    async fn tool_loop_runs_function_call_then_final() {
        // Needs a real DB for agent_runs — skip if DATABASE_URL unset.
        let Ok(url) = std::env::var("DATABASE_URL") else {
            return;
        };
        let Ok(db) = sqlx::PgPool::connect(&url).await else {
            return;
        };
        let project_id = Uuid::now_v7();
        let owner = format!("agent-test-{project_id}");
        if sqlx::query("insert into app_users (id, email) values ($1, $2) on conflict do nothing")
            .bind(&owner)
            .bind(format!("{owner}@test.local"))
            .execute(&db)
            .await
            .is_err()
        {
            return;
        }
        if sqlx::query(
            "insert into projects (id, slug, name, owner_user_id) values ($1, $2, $3, $4) \
             on conflict do nothing",
        )
        .bind(project_id)
        .bind(format!("agent-{project_id}"))
        .bind("Agent test")
        .bind(&owner)
        .execute(&db)
        .await
        .is_err()
        {
            return;
        }

        let model = ScriptedModel {
            turns: Mutex::new(vec![
                AgentTurn::FunctionCalls {
                    calls: vec![FunctionCall {
                        call_id: "c1".into(),
                        name: "echo".into(),
                        arguments: r#"{"x":"hi"}"#.into(),
                    }],
                    echo_items: vec![],
                    usage: None,
                },
                AgentTurn::Final {
                    text: r#"{"answer":"done"}"#.into(),
                    usage: None,
                },
            ]),
        };
        let tool = EchoTool;
        let tools: Vec<&dyn AgentTool> = vec![&tool];
        let ctx = AgentCtx {
            project_id,
            platform: Platform::Instagram,
            plan_id: None,
            db,
            state: None,
        };
        let prompt = RenderedPrompt {
            system: "sys".into(),
            user: "user".into(),
            version: "1".into(),
        };
        let (out, _run_id) = run_tool_loop::<TinyOut>(
            &model,
            TextRole::Planner,
            "test_loop",
            &prompt,
            &tools,
            &ctx,
            Effort::Low,
            ToolLoopLimits {
                max_steps: 5,
                max_output_tokens: 1000,
            },
        )
        .await
        .expect("loop");
        assert_eq!(out.answer, "done");
    }

    #[test]
    fn tool_defs_shape() {
        let tool = EchoTool;
        let defs = tool_defs(&[&tool]);
        assert_eq!(defs[0]["type"], "function");
        assert_eq!(defs[0]["name"], "echo");
    }
}
