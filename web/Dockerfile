FROM rust:1.91-bookworm AS builder

RUN cargo install wasm-pack --locked

WORKDIR /src
COPY . .
RUN wasm-pack build web --target web --out-dir pkg --release --no-pack

FROM python:3.13-alpine

WORKDIR /srv
COPY --from=builder /src/web/index.html /src/web/app.js /src/web/scan-worker.js /src/web/style.css ./
COPY --from=builder /src/web/server.py ./
COPY --from=builder /src/web/pkg ./pkg

EXPOSE 8000
CMD ["python3", "server.py"]
