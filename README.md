<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/vishalkrishnads/skywoop/main/public/logos/logo-dark.png">
    <source media="(prefers-color-scheme: light)" srcset="https://raw.githubusercontent.com/vishalkrishnads/skywoop/main/public/logos/logo-light.png">
    <img alt="Skywoop" src="https://raw.githubusercontent.com/vishalkrishnads/skywoop/main/public/logos/logo-light.png" width="96" height="96" />
  </picture>
</p>

<h1 align="center">Skywoop Server</h1>

<p align="center">
  <strong>High-performance Rust backend for the Skywoop autonomous video marketing pipeline.</strong><br />
  Powers plan synthesis, asset storage tickets, daypart scheduling, and Meta Graph API syndication.
</p>

<p align="center">
  <a href="https://skywoop.com"><strong>🚀 Try Live Application: skywoop.com &raquo;</strong></a>
</p>

<p align="center">
  <a href="https://skywoop.com"><img src="https://img.shields.io/badge/Production%20App-skywoop.com-000000?style=for-the-badge&logo=apple&logoColor=white" alt="Live App at skywoop.com" /></a>
  <a href="https://github.com/vishalkrishnads/skywoop"><img src="https://img.shields.io/badge/Frontend%20Repo-skywoop-black?style=for-the-badge&logo=nextdotjs&logoColor=white" alt="Next.js Frontend Repository" /></a>
</p>

<p align="center">
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/Rust-2024%20(1.94%2B)-dea584?style=flat-square&logo=rust&logoColor=white" alt="Rust 2024" /></a>
  <a href="https://github.com/tokio-rs/axum"><img src="https://img.shields.io/badge/Axum-0.8-blue?style=flat-square" alt="Axum 0.8" /></a>
  <a href="https://tokio.rs"><img src="https://img.shields.io/badge/Tokio-1.0-orange?style=flat-square" alt="Tokio" /></a>
  <a href="https://github.com/launchbadge/sqlx"><img src="https://img.shields.io/badge/SQLx-0.9%20Postgres-00E599?style=flat-square&logo=postgresql&logoColor=black" alt="SQLx Postgres" /></a>
  <a href="https://azure.microsoft.com"><img src="https://img.shields.io/badge/Azure-Container%20Apps-0078D4?style=flat-square&logo=microsoftazure&logoColor=white" alt="Azure Container Apps" /></a>
  <a href="https://swagger.io/specification/"><img src="https://img.shields.io/badge/OpenAPI-3.1-85EA2D?style=flat-square&logo=openapiinitiative&logoColor=black" alt="OpenAPI 3.1" /></a>
</p>

---

> ### 🌐 Production Service
> - **Production API**: [https://api.skywoop.com](https://api.skywoop.com)
> - **Live Web Studio**: [https://skywoop.com](https://skywoop.com)
> - **Frontend Web Repository**: [github.com/vishalkrishnads/skywoop](https://github.com/vishalkrishnads/skywoop)

---

## 💡 The Problem & The Idea

### The Founder's Dilemma
Solo iOS developers and indie hackers spend **90% of their creative energy writing Swift code** and **0% producing daily video content**. Yet today, algorithmic organic discovery on platforms like **Instagram Reels**, **TikTok**, and **Facebook Watch** is the single highest-converting distribution channel for mobile applications.

Creating daily video content manually requires:
- Brainstorming viral hooks and script variations every morning.
- Capturing screen recordings, editing 1080&times;1920 vertical canvas videos, and timing audio effects.
- Animating kinetic captions, keyframe zooms, and sound transitions.
- Manually exporting, transferring to mobile devices, and posting with captions and hashtags.

**Most indie developers quit after 3 days.**

### The Skywoop Solution
**Skywoop replaces the entire manual content creator department with an autonomous daily video pipeline:**

1. **Ingestion Without Configuration**: You simply provide your public App Store URL. Skywoop automatically scrapes and extracts your application name, branding colors, typography, app screenshots, feature highlights, and customer reviews.
2. **AI Daily Plan Synthesis**: Every morning, the AI planner drafts a targeted 1080&times;1920 vertical video plan containing validated hooks, timed flowcharts, kinetic captions, and call-to-actions.
3. **1-Click Founder Approval**: You wake up, open the desktop or mobile dashboard, review the day's proposal, request any tweaks via conversational AI agent, and click approve.
4. **Autonomous Remotion Studio Render**: The browser composition engine compiles real app recordings, generative AI avatars, B-roll, background audio, sound effects, and frame-perfect captions into production-grade 60fps MP4s.
5. **Algorithmic Multi-Post & Analytics Compounding**: Scheduled dispatches automatically syndicate via verified Meta Graph APIs to Instagram and Facebook. As engagement data rolls in, Skywoop updates its memory to double down on high-converting hooks.

---

## ⚙️ The Role of `skywoop-server`

This repository houses the **Rust backend service** (`skywoop-server`). It acts as the high-throughput operational backbone of Skywoop, responsible for:

* **Stateless EdDSA Authentication**: Decodes and verifies asymmetric JSON Web Tokens minted by Better Auth on the Next.js frontend via live JWKS validation.
* **Domain Persistence**: Manages projects, audiences, brand palettes, daily plan revisions, chat threads, and playbooks backed by PostgreSQL.
* **Storage Ticket Provisioning**: Generates pre-signed upload and download SAS tokens for Azure Blob Storage (or local storage in development).
* **Meta Graph API Syndication**: Manages long-lived user and page access tokens, Instagram Business account linking, container creation, video upload, and status polling.
* **Autonomous Task Scheduling & Background Workers**: Drives the daily plan generation cycle, automated morning notifications, and App Store Connect analytics syncing.
* **Social Trend Intelligence**: Ingests trending hashtags, audio tracks, and viral formats across target geos.

---

## 🛠️ Technology Stack & Tools

| Component | Technology | Role |
|---|---|---|
| **Language** | **Rust 2024 (1.94+)** | Guarantees memory safety, uncompromising concurrency, zero garbage collection pauses, and minimal memory footprint. |
| **HTTP Framework** | **[Axum 0.8](https://github.com/tokio-rs/axum)** | Ergonomic, composable web framework built on top of `tokio`, `tower`, and `hyper`. |
| **Async Runtime** | **[Tokio](https://tokio.rs)** | Multi-threaded work-stealing async scheduler powering high-throughput API requests and background worker loops. |
| **Database & ORM** | **[SQLx 0.9](https://github.com/launchbadge/sqlx)** | Compile-time checked asynchronous SQL queries and migrations for PostgreSQL. |
| **Authentication** | **[jsonwebtoken](https://github.com/Keats/jsonwebtoken)** | In-memory cached EdDSA JWKS verifier validating tokens issued by Next.js / Better Auth. |
| **Cloud Storage & Secrets** | **Azure SDK for Rust** | First-party crates for Azure Blob Storage, Azure Key Vault Secrets, and Entra ID Managed Identity. |
| **OpenAPI & Docs** | **[Utoipa 6](https://github.com/juhaku/utoipa)** | Compile-time type-safe OpenAPI 3.1 schema generation and documentation endpoints. |
| **HTTP Client** | **[Reqwest 0.13](https://github.com/seanmonstar/reqwest)** | Async HTTP client with rustls and streaming support for Meta Graph, App Store Connect, and AI endpoints. |
| **Prompt Templates** | **[Minijinja 2](https://github.com/mitsuhiko/minijinja)** | Jinja2-compatible zero-dependency templating engine for LLM prompt generation. |
| **Telemetry & Observability** | **Tracing & OpenTelemetry** | Structured JSON logging with trace context propagation and OTLP export. |

---

## 🏛️ System Architecture

```
                               ┌──────────────────────────────────────────────┐
                               │       Next.js 16 Web Client (Browser)        │
                               │             https://skywoop.com              │
                               └──────────────────────┬───────────────────────┘
                                                      │
                                                      ▼ (EdDSA Signed Bearer JWT)
    ┌─────────────────────────────────────────────────────────────────────────────────────────────────┐
    │                                     skywoop-server (Rust)                                       │
    │                                                                                                 │
    │  ┌───────────────────────┐   ┌───────────────────────────┐   ┌───────────────────────────────┐  │
    │  │   Axum HTTP Router    │   │  Background Job Worker    │   │      Data Job Scheduler       │  │
    │  │  • /api/v1/projects   │   │  • Asset processing       │   │  • Daily plan generation      │  │
    │  │  • /api/v1/plans      │   │  • Meta Reel publishing   │   │  • Trend crawls (US/Global)   │  │
    │  │  • /api/v1/oauth      │   │  • App Store Connect sync │   │  • Morning dispatch queue     │  │
    │  └───────────┬───────────┘   └─────────────┬─────────────┘   └───────────────┬───────────────┘  │
    │              │                             │                                 │                  │
    │              ▼                             ▼                                 ▼                  │
    │  ┌───────────────────────────────────────────────────────────────────────────────────────────┐  │
    │  │                                  AppState Domain Layer                                    │  │
    │  │         JwksCache • SqlxPool • AzureBlobStore • KeyVaultSecrets • MetaClient              │  │
    │  └─────────────────────────────────────────┬─────────────────────────────────────────────────┘  │
    └────────────────────────────────────────────┼────────────────────────────────────────────────────┘
                                                 │
                   ┌─────────────────────────────┼─────────────────────────────┐
                   ▼                             ▼                             ▼
    ┌───────────────────────────┐ ┌───────────────────────────┐ ┌───────────────────────────┐
    │     PostgreSQL 18+        │ │    Azure Cloud Services   │ │    Meta Graph & Apple ASC   │
    │ • SQLx Migrations         │ │ • Azure Blob Storage      │ │ • Instagram Reels API       │
    │ • Projects, Plans, Brands │ │ • Azure Key Vault Secrets │ │ • Facebook Pages Video API  │
    │ • Video Revisions & Assets│ │ • Azure Container Apps    │ │ • App Store Connect API     │
    └───────────────────────────┘ └───────────────────────────┘ └───────────────────────────┘
```

---

## 💻 Getting Started Locally

### Prerequisites

* **Rust**: `1.94+` (install via `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`)
* **Docker & Docker Compose**: For running a local PostgreSQL instance
* **Next.js Client (Optional)**: Running on `http://localhost:3000` from the [skywoop frontend repository](https://github.com/vishalkrishnads/skywoop)

### 1. Clone the repository

```bash
git clone https://github.com/vishalkrishnads/skywoop-server.git
cd skywoop-server
```

### 2. Start local PostgreSQL

A preconfigured `docker-compose.yml` is provided for local database development:

```bash
docker compose up -d postgres
```

This starts PostgreSQL on port `5432` with username `skywoop`, password `skywoop`, and database `skywoop`.

### 3. Setup environment configuration

Create a `.env` file from the example:

```bash
cp .env.example .env
```

Key environment configurations for local development:

```env
APP_ENV=local
LISTEN_ADDR=0.0.0.0:8080
PUBLIC_BASE_URL=http://localhost:8080
APP_ORIGIN=http://localhost:3000
DATABASE_URL=postgres://skywoop:skywoop@localhost:5432/skywoop

# ── Authentication Mode ───────────────────────────────────────────────────────
# Option A: Standalone development with a static bearer token:
AUTH_MODE=dev_token
DEV_TOKEN=dev-secret-token-change-me

# Option B: End-to-end integration with the Next.js frontend:
# AUTH_MODE=jwt
# AUTH_JWKS_URL=http://localhost:3000/api/auth/jwks
# AUTH_ISSUER=http://localhost:3000
# AUTH_AUDIENCE=skywoop-api

# ── Local Storage & Secrets ──────────────────────────────────────────────────
LOCAL_ASSET_DIR=./.data/assets
LOCAL_SECRETS_FILE=./.data/secrets.json

# ── AI Model Deployments (Azure OpenAI) ───────────────────────────────────────
AZURE_OPENAI_BASE=https://your-resource.openai.azure.com/openai/v1
AZURE_OPENAI_API_KEY=your_azure_key_here
DEPLOYMENT_PLANNER=gpt-5.5
DEPLOYMENT_VISION=gpt-5.5
DEPLOYMENT_COPY=gpt-5.5-mini
DEPLOYMENT_IMAGE_DRAFT=gpt-image-2.5
DEPLOYMENT_IMAGE_FINAL=gpt-image-2.5
```

### 4. Run database migrations

Apply SQLx migrations to set up the database schema:

```bash
cargo run -- migrate
```

### 5. Launch the server

```bash
cargo run -- serve
```

The server will start listening on `http://0.0.0.0:8080`.

---

## 🛠️ CLI Commands & Utilities

The `skywoop` binary provides several built-in subcommands:

```bash
# Run HTTP API server, job workers, and scheduler
cargo run -- serve

# Apply pending SQLx migrations to DATABASE_URL and exit
cargo run -- migrate

# Output the complete OpenAPI 3.1 JSON specification
cargo run -- openapi > openapi.json

# Test Azure OpenAI structured output generation
cargo run -- ai-smoke

# Fetch and print today's trending audio & topics for Instagram
cargo run -- trends --country US --platform instagram
```

---

## 🧪 Testing and Quality

```bash
# Run all unit and integration tests
cargo test

# Check code against Clippy lints
cargo clippy

# Verify build
cargo check
```

---

## 📁 Repository Structure

```
skywoop-server/
├── src/
│   ├── main.rs              # CLI entry point (serve, migrate, openapi, trends)
│   ├── router.rs            # Axum route definitions, CORS, & middleware
│   ├── state.rs             # AppState dependency injection container
│   ├── config.rs            # Typed environment configuration
│   ├── agent/               # Autonomous AI director & conversational plan editor
│   ├── ai/                  # Azure OpenAI client, structured outputs, & prompts
│   ├── appstore/            # App Store Connect API client & metadata scraper
│   ├── auth/                # JWT/JWKS verification & dev-token middleware
│   ├── azure/               # Managed Identity, Key Vault, & Blob Storage
│   ├── db/                  # SQLx connection pools & transaction helpers
│   ├── domain/              # Typed models (projects, plans, assets, schedules)
│   ├── jobs/                # Background worker queues, scheduler, & runners
│   ├── meta/                # Meta Graph API client (Instagram & Facebook)
│   ├── storage/             # Blob storage abstraction (Local vs Azure Blob)
│   └── trends/              # Social media trend intelligence crawler
├── migrations/              # SQL migrations applied via SQLx
├── prompts/                 # Jinja2 prompt templates for video plans & copy
├── templates/               # Meta caption & flowchart templates
├── scripts/
│   ├── deploy.sh            # Production deployment script for Azure Container Apps
│   ├── bootstrap-prod.sh    # Production infrastructure setup
│   └── reset-db.sh          # Local database cleanup & re-migration
├── docs/
│   ├── API.md               # Detailed API route specification
│   └── PLAN_FORMAT.md       # Markdown plan DSL & AST documentation
├── Dockerfile               # Multi-stage distroless production Docker image
└── azure.yaml               # Azure Developer CLI configuration
```

---

## 🌐 Production Deployment

The production server runs as an **Azure Container App** (`skywoop-api`) in `rg-skywoop-prod`.

### Multi-Stage Docker Build
The service is packaged into a secure, minimal container using `gcr.io/distroless/cc-debian12`:

```bash
docker build -t skywoop-api:latest .
```

### Automated Azure Deployment
Deployments are handled via the included deployment script:

```bash
# Requires Azure CLI (az) and Docker
./scripts/deploy.sh
```

This script:
1. Authenticates with Azure Container Registry (ACR).
2. Builds and pushes the `linux/amd64` container image.
3. Updates the `skywoop-api` Container App revision with zero downtime.
4. Automatically runs SQLx migrations on startup.
5. Verifies health via `https://api.skywoop.com/healthz`.

---

## 👥 Related Repositories

* **Live Web Application**: [skywoop.com](https://skywoop.com)
* **Web Frontend Repo**: [`vishalkrishnads/skywoop`](https://github.com/vishalkrishnads/skywoop) &mdash; Next.js 16, Remotion 4, Better Auth, and Tailwind CSS v4.

---

## 📄 License

This project is licensed under the [MIT License](LICENSE).
