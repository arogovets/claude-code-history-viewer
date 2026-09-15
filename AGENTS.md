# Agent Runbook

## After Feature Implementation

When a feature changes the WebUI server or frontend served by it:

1. Rebuild the server binary:

   ```bash
   cargo build --release --features webui-server
   ```

2. Restart the macOS LaunchAgent:

   ```bash
   launchctl kickstart -k "gui/$(id -u)/com.cchv"
   ```

3. Verify that the service is running and the server responds:

   ```bash
   launchctl print "gui/$(id -u)/com.cchv"
   curl -sS -i --max-time 5 http://127.0.0.1:3727/health
   ```

4. Verify the Kanban API:

   ```bash
   curl -sS -i --max-time 10 \
     -X POST http://127.0.0.1:3727/api/load_kanban \
     -H 'content-type: application/json' \
     --data '{}'
   ```

Expected results are a running `com.cchv` service, `200 {"status":"ok"}` from
`/health`, and a successful JSON response from `/api/load_kanban`.

If the service is still starting, wait briefly and retry the checks. For a
startup failure, inspect `/Users/emac/.claude-history-viewer/cchv-daemon.log`
and the `last exit reason` from `launchctl print`.
