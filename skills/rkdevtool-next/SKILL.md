---
name: rkdevtool-next
description: Connect to the local RKDevTool Next desktop app over MCP to inspect Rockchip hardware and firmware, back up partitions, or carry out user-requested flashing and recovery.
---

# RKDevTool Next

## Connect and verify

1. Run the desktop app or `rkdevtool-next-cli serve` on the same computer as the MCP client. Use the connection address and local configuration path included below or printed by the CLI. For desktop, the user can copy these from **Preferences → MCP Server**.
2. Use the registered tools if available, or import the generated JSON with a client that supports it (e.g. `claude --mcp-config <file>`). If Codex desktop has not registered the server, call it from a terminal on the same computer with MCPorter 0.17.0 (Node.js 24+):

   ```bash
   config="/absolute/path/to/rkdevtool-next.mcp.json"
   npx --yes mcporter@0.17.0 --config "$config" call rkdevtool-next.get_runtime_info
   npx --yes mcporter@0.17.0 --config "$config" call rkdevtool-next.scan_devices
   ```

   MCPorter reads the config directly; do not extract or pass the authorization header yourself. If the terminal cannot reach the host's loopback interface, explain the blocker; do not create a tunnel.
3. Verify the connection with `get_runtime_info` before working with the device. Do not write to a device just to test it.

## Work with a device

- Start with `scan_devices`. Use the current ID, rescan after any reconnect, and do not choose between multiple devices on the user's behalf. In Maskrom, do not infer the chip from the USB PID. In Loader, check the reported chip and capacity.
- Inspect a firmware with `inspect_firmware`, or use `inspect_archive` to pick the exact archive member. Verify board and storage compatibility before requesting a write. For partition operations use the exact name, start, and length from `read_partitions`; re-read after a storage switch. Sectors are 512 bytes.
- For an authorized mutation, summarize the target and impact and submit once. Wait for approval if the desktop prompts; with MCPorter, use `--timeout 120000` for `start_operation` to allow time for approval. Flash jobs (including a requested post-flash reboot) can start without a prompt only if the user enabled automatic flash in desktop settings or started the CLI server with `--allow-flash`. The headless server refuses other operations that require approval. Poll `get_operation` with the returned job ID. Do not blindly retry a timed-out request. Cancellation does not undo written sectors. Report failed or cancelled writes before attempting further operations.
