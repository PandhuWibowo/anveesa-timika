.PHONY: dev stop backend frontend install build test up down redis redis-stop redis-logs redis-cli \ gen e2e
	raft-dev raft-reset raft-up raft-down helm-lint helm-template \
	redis-sentinel-up redis-sentinel-down redis-cluster-up redis-cluster-down \
	scale-up scale-down swarm-redis swarm-raft swarm-rm

# Backend port must match BIND_ADDR in backend/.env so `make stop` can free it.
BACKEND_PORT ?= 8200
FRONTEND_PORT ?= 5174

# Local Redis storage engine. Matches REDIS_URL in backend/.env.
REDIS_CONTAINER ?= timika-redis
REDIS_PORT ?= 6380

# Run backend + frontend together in one terminal. Ctrl+C stops both.
dev:
	@trap 'kill 0' EXIT INT TERM; \
	(cd backend && cargo run) & \
	(cd frontend && bun run dev) & \
	wait

# Kill any process still bound to the dev ports (e.g. after a crash/Ctrl+C
# that left orphans behind).
stop:
	@for port in $(BACKEND_PORT) $(FRONTEND_PORT) 8201 8210 8211 8220 8221; do \
		pid=$$(lsof -ti tcp:$$port); \
		if [ -n "$$pid" ]; then \
			echo "Killing process on port $$port (pid $$pid)"; \
			kill -9 $$pid; \
		else \
			echo "Nothing listening on port $$port"; \
		fi; \
	done

backend:
	cd backend && cargo run

frontend:
	cd frontend && bun run dev

install:
	cd frontend && bun install

build:
	cd backend && cargo build --release
	cd frontend && bun run build

# TypeScript gRPC-Web client code from proto/ (the backend's build.rs does Rust).
gen:
	cd frontend && bun run gen

test: gen
	cd backend && cargo test
	cd frontend && bun run check

# 200 end-to-end scenarios against real processes (fresh Raft nodes, an SSH test
# target, gRPC / gRPC-Web / HTTP / WebSocket / CLI). Report: frontend/e2e/REPORT.md.
# Filter: make e2e ONLY="E F07"
e2e:
	cd backend && cargo build && cargo build --example ssh_target
	cd frontend && bun e2e/run.ts $(ONLY)

up:
	docker compose up --build

down:
	docker compose down

# Start a local Redis configured the way a vault needs it (idempotent):
# AOF on with per-second fsync, and NO eviction. Data persists in a named
# volume; `make redis-stop` removes the container, not the volume.
redis:
	@if [ -z "$$(docker ps -aq -f name=^$(REDIS_CONTAINER)$$)" ]; then \
		echo "Starting Redis container $(REDIS_CONTAINER) on :$(REDIS_PORT)"; \
		docker run -d --name $(REDIS_CONTAINER) -p $(REDIS_PORT):6379 \
			-v timika-redis-data:/data redis:7-alpine \
			redis-server --appendonly yes --appendfsync everysec --maxmemory-policy noeviction; \
	else \
		echo "Reusing existing $(REDIS_CONTAINER); ensuring it is running"; \
		docker start $(REDIS_CONTAINER) >/dev/null; \
	fi

redis-stop:
	@docker rm -f $(REDIS_CONTAINER) 2>/dev/null && echo "Removed $(REDIS_CONTAINER)" || echo "No $(REDIS_CONTAINER) container"

redis-logs:
	docker logs -f $(REDIS_CONTAINER)

# Look at what's actually stored — you should only ever see ciphertext.
redis-cli:
	docker exec -it $(REDIS_CONTAINER) redis-cli

# ─── Scale-out (docs/SCALING.md) ──────────────────────────────────────────────
# N stateless replicas on Redis behind a seal-aware HAProxy on :8200.
REPLICAS ?= 3
scale-up:
	docker compose -f deploy/scale/docker-compose.yml up -d --build --scale timika=$(REPLICAS)

scale-down:
	docker compose -f deploy/scale/docker-compose.yml down -v

# Docker Swarm stacks (needs `docker swarm init`).
swarm-redis:
	docker stack deploy -c deploy/swarm/stack.redis.yml timika

swarm-raft:
	docker stack deploy -c deploy/swarm/stack.raft.yml timika

swarm-rm:
	docker stack rm timika

# ─── Redis HA test rigs (timika runs inside the network, UI/API on :8200) ─────
redis-sentinel-up:
	docker compose -f deploy/redis/docker-compose.sentinel.yml up --build

redis-sentinel-down:
	docker compose -f deploy/redis/docker-compose.sentinel.yml down -v

redis-cluster-up:
	docker compose -f deploy/redis/docker-compose.cluster.yml up --build

redis-cluster-down:
	docker compose -f deploy/redis/docker-compose.cluster.yml down -v

# ─── Integrated Raft ──────────────────────────────────────────────────────────
# 3 local nodes (api :8200/:8210/:8220, cluster :8201/:8211/:8221), no Docker.
# Run `make frontend` alongside: the UI talks to timika-0 on :8200.
raft-dev:
	./scripts/raft-dev.sh

raft-reset:
	RESET=1 ./scripts/raft-dev.sh

# The same 3-node cluster in containers, one volume per node.
raft-up:
	docker compose -f docker-compose.raft.yml up --build

raft-down:
	docker compose -f docker-compose.raft.yml down

helm-lint:
	helm lint deploy/helm/timika

# Render plain Kubernetes manifests (for kubectl apply / kustomize users).
helm-template:
	helm template timika deploy/helm/timika --namespace timika
