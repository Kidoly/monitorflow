# monitorflow-e2e

Test-only crate: one end-to-end test (`tests/e2e.rs`) that runs the real
`monitorflow-server` and `monitorflow-agent` binaries the way an operator
would. Nothing here is shipped.

The scenario, in a temp dir with random loopback ports and a database created
for the run (and dropped afterwards, failure included):

1. write a server config from scratch, start the server, wait for `/api/health/ready`;
2. `user add` an administrator (password on stdin), `token create` for agent `e2e-01`;
3. write a minimal agent config with the token, start the agent;
4. log in through the API and poll `GET /api/servers` until `e2e-01` is online
   with the host's hostname and the fake os-release's family and version;
5. SIGTERM the agent and see it go offline; restart it without a token and see
   it come back with its stored identity;
6. SIGTERM the server and the agent and check both exit with status 0, and that
   no log contains the token secret or the password.

Child processes are killed if the test fails, and their logs are printed.

## How the binaries are found

Cargo only exposes `CARGO_BIN_EXE_<name>` to the tests of the package that owns
the binary, and artifact dependencies are still unstable. The test therefore
runs `cargo build -p monitorflow-server -p monitorflow-agent --bins
--message-format=json` itself, with the `CARGO` that runs the test, the same
target directory (derived from `CARGO_TARGET_TMPDIR`) and the same profile, and
takes the executable paths from cargo's JSON messages. `cargo test -p
monitorflow-e2e` works on its own, and under `cargo test --workspace` the build
is nearly free. `escargot` does the same thing; it was not worth a dependency.

## Running it

Like the other PostgreSQL tests it is ignored unless `DATABASE_URL` is set at
build time (`build.rs`):

```
docker compose -f deploy/docker-compose.dev.yml up -d postgres
DATABASE_URL=postgres://monitorflow:monitorflow@localhost:5432/monitorflow cargo test -p monitorflow-e2e
```

The agent only starts on debian and rhel family distributions. The test
writes a fake debian os-release in its temp dir and points the agent at it
with `MONITORFLOW_OS_RELEASE_PATH` (a variable meant for tests only), so it
runs on any Linux host.
