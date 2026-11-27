FROM rust:1.88-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY recipe-app ./recipe-app
RUN cargo build --locked --release -p recipe-app

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 recipes && useradd --uid 10001 --gid 10001 --no-create-home recipes \
    && mkdir -p /app /data && chown 10001:10001 /data
WORKDIR /app
COPY --from=build /src/target/release/recipe-app /app/recipe-app
# The application embeds templates, fonts, guidelines, migrations and CSS at build time.
USER 10001:10001
ENV APP__BIND_ADDR=0.0.0.0:3000 APP__DATABASE_PATH=/data/recipes.sqlite3 APP__CONTENT_DIR=/data/content
EXPOSE 3000
ENTRYPOINT ["/app/recipe-app"]
