FROM rust:1.94-bookworm AS build
WORKDIR /app
COPY . .
ENV SQLX_OFFLINE=true
RUN cargo build --release --locked

FROM gcr.io/distroless/cc-debian12
COPY --from=build /app/target/release/skywoop /skywoop
COPY --from=build /app/prompts /prompts
COPY --from=build /app/templates /templates
EXPOSE 8080
ENTRYPOINT ["/skywoop", "serve"]
