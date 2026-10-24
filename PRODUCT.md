# Skywoop

We're seeing creatives on Instagram and Facebook not pick up steam. The problem seems to be that all of these are strictly image creatives. However, this is not hooky enough. So, we're moving to video creatives in the next phase. This hand-written document now details the new product idea.

## Problem
There are many solo or small team app developers with very niche and great app ideas. However, they're very focused on building their app, as they should be, and fail at getting users.
The fact is that for consumer apps today, distribution is the moat. So, any push developers can get to grow their app is substantial to their app becoming successful. Right now, competitors like Higgsfield Supercomputer exist but they don't really cater to this market effectively. They're basically wrappers for AI video models. Agent native video editors like Cardboard exist too but they also cater to a different market, not this specifically.

## Solution (MVP)
The MVP only caters to iOS developers who have launched on the app store and are targeting advertisement using meta platforms (IG & FB).&#x20;

### Onboarding
Once a user signs up, we crawl their website (if available) and their App Store listing to generate a description of their app, get the app's logo and app store screenshots. Then, we ask the user to add preliminary assets for their creatives, like screen recordings and screenshots of their apps, any AI characters they may have generated earlier, etc.

### Daily loop
- An AI agent drafts a plan for a given day. The plan will detail what creatives (videos only) are going to be posted that day. The creatives themselves won't be generated but would be represented using flowcharts. The concept behind each creative will be detailed.
- The user reads the plan and approves it. If the agent requires specific assets to make a certain creative, the user can attach them or allow the agent to use video gen to create said assets. The user can steer the agent through a chat UI and also leave specific comments in the plan itself.
- Once approved, the agent uses an agent friendly video editor plus image gen or video gen models to build the creative. A total of 10 creatives will be posted per day. 5 of them intended to capture new audiences and 5 to piggyback off of the day's hottest trends.
- The creatives are posted at different times of day, according to which times the intended geography is most active on Instagram or Facebook.
- After 24-48 hours, the app store reports any new growth figures and Meta reports the performance figures of each creative. The agent makes a diary entry with the new data.
- Next day, the agent reads the previous diary entries and drafts a new plan. Loop repeats.

The user will be able to adjust the number of creatives and everything else to stay under a cost budget.

### Asset manager
Skywoop includes a proper asset manager with directories and subdirectories for each user. When a user uploads anything (even to the plan), we generate a semantic description of the asset such that agents can refer to them later. Each directory and subdirectory can also have descriptions. These descriptions can be updated by any agent working on any task.

The user can mention any asset by filename using the `@` directive in the chat UI when steering the agent. The idea is that these assets can be used directly by the agent in it's video editor when generating the creative vs having a video gen model regenerate them and hallucinate the result. This way, we can create very accurate UGC's.

### Agentic video editor
In Skywoop, videos are primarily ***edited***  by an LLM, as opposed to Higgsfield supercomputer in which the LLM **directs** the video.

The difference is that we'll have a cardboard style video editor built in to our stack that can be read and used completely by the agent autonomously. Higgsfield can, at best, make a plan for the videos using an LLM, generate frames for the end video, and hope that the video gen model doesn't hallucinate the result. We don't have that. Frontier LLMs have much better context awareness and way less hallucination. And since they're woking with clearly labelled assets all the time, videos come out accurately most of the time. And when something does go wrong, the user's comment on the plan can help the agent easily pinpoint the error using the available editor tools.

### Our hidden edge
We maintain a continuously updated playbook for our agents. If left barely to generate creatives, AI agents usually resort to making the most generic creatives ever with a lot of scenes or motions which makes them or the video gen models hallucinate and come up with straight up bad videos. However, for specific types of apps, there are specific types of creatives that work the most. More importantly, these creatives can be made with simple shots (without varying camera angles) and clever screenshots (with reduced opacity if required for see through) + captions overlaid properly.

For instance, in order for a screen time app to get better conversion, it would be best to post a girl scrolling away on her phone with a lot of books open on her desk. A set of screenshots showing screen times being 10-15 hours can be overlaid (cycled through) with a low enough opacity for see through. The caption can be "I couldn't get off Instagram even for exams" at that point. Then, switch to her studying with her phone beside on the same desk. The overlaid screenshots now show her screen time at \~35-45m per day. The caption can be "But then I found \<app name>". This converts way better than "I used to scroll Instagram daily, but now I don't".

When one user's agent learns such a playbook worthy trick, it sends the info to the central server where we review and approve it for all agents reading the playbook.

## A day's workflow

Each day, the client will get a fully written plan from the server. Like now, there shall be two plans for Instagram & Facebook separately. Remember, the plan will be strictly text only. The flowcharts for individual videos would be illustrated by the LLM using text and the client would render them beautifully. To do this, the server will supply any available data to the LLM from previous days and it would write the plan, along with a short generated greeting text for the user regarding that plan. Eg: "Hey, today we'll experiment with a new hook: a girl trying to study. Please read the plan thoroughly and let me know what the next steps are." The plan itself shall be a markdown file.

Beyond this, all AI & agentic infra will be in the client.

When the user navigates to the instagram route, they'll see the UI split into two parts vertically. The left half will have a traditional LLM chat interface and the right half will render the plan.
Initially,

* Left half shows a chat box, with the generated greeting text and a Start button. This button would initially be disabled.
* Right half details the plan. The user shall be able to leave comments under each of the video idea flowcharts. This can be facilitated with the UI since markdown doesn't support it natively. 
* The user's comments should be included in the final raw markdown file (w/ proper annotation) for the agent to read. The agent should be able to read the complete markdown file with the user's comments annotated as User commented <comment>.
* The agent has tools for properly searching through the user's assets using the asset manager. Since each asset is already described properly for what it is, the agent has tools to search using the descriptions, or directly look through the media in assets to confirm if required. Directly looking through media can be discouraged for cost optimization. Under each video idea flowchart, the agent will list what assets are required (along with type video, audio or image) and which are available. These would be rendered as a checklist. Beside each item in the checklist, the user will have two options available: Upload or Generate.
* If the user uploads, mark that checklist item as done. If they choose generate, then show a pending timer icon there. If all checklist items (across all videos) have been marked either done or pending, the Start button will illuminate near the chat box.
* Once the user clicks Start, instruct the main agent to generate all the pending assets using available models. Spin up subagents to do these tasks. These subagents would generate the assets and place them in the asset manager with apt and suitable descriptions. Once a subagent returns, mark that checklist item done. Once all checklist items are done, then the Start button changes to Build.
* Once the user starts chatting with the agent, the chat box and the UI on the left respond accordingly. The agent has tools to read certain lines in the plan or read the plan fully. It has tools to make targeted edits in the plan. It also has a tool to ask a question in the chat. It accepts a question and multiple options for answers as input, and produces a single answer as output. The tool displays the question in the UI and the answers as clickable options for the user. If the question tool isn't being used or if the agent isn't actively working, then display the Start/Build button near the chat box.

Once the user confirms and clicks Build, the agent spins up multiple subagents to create the videos in parallel. By this point, the main agent will have a clearcut idea as to how each video should come out. Each subagent will get a very descriptive instruction to make this happen using our new agentic video editor. After all videos are done,

* The plan shows the video instead of the flowchart. We're not storing the generated videos themselves in the asset manager. The video will be in our storage only until it's posted.
* There shall be an edit button near each video in the plan. The video can be played right there from the plan too.
* If the edit button is clicked, we navigate the user to a new page which is our video editor.
* The video editor will have 3 main panes: the timeline, assets and the video preview. There shall be a chat interface occupying ~30% of available width on it's right. However, it can be dismissed/opened with a button click. If the chat pane is open, then preview & assets appear at the top half, split vertically and timeline appears in the bottom half. If not, then preview appears on the left ~30% width and timeline + assets appear on the right, split horizontally.
* The user can manually edit the video or ask an LLM on the right to do it for them. This editor shall be minimal. It allows: adding text & captions to the video, overlaying a video inside another video, editing the overlaid video (tilt, zoom, etc). Most importantly, an LLM (not a video gen model) should be able to read and write to this editor natively. In other words, it should be able to use it like any other MCP tool to edit a video properly, just like a human would. This is the challenge with the editor.

Once the user clicks Schedule, the posts are scheduled for the next cycle. The plan can't be edited further. 
In the same planning UI, the user can switch the date to a previous date using a UI element at the top left. However, those plans won't be editable either and once the videos have been posted to the platform and deleted from the system, they won't be shown in the plan. Just the links will be available.

The plan for the next day shall only be created after proper data is available for the previous day.