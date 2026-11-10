---
name: rkdevtool-next
description: Connect to the local RKDevTool Next desktop app over MCP to inspect Rockchip hardware and firmware, back up partitions, or carry out user-requested flashing and recovery.
---

# RKDevTool Next

## Connect and verify

1. Run the desktop app or `rkdevtool-next-cli serve` on the same computer as the MCP client. Use the connection address and local configuration path included below or printed by the CLI. For desktop, the user can copy these from **Preferences → MCP Server**.
2. Register the `mcpServers.rkdevtool-next` entry from that generated JSON using the client's supported MCP configuration. It provides the Streamable HTTP URL and authorization header. Use native file import when available (e.g. `claude --mcp-config <file>` for a Claude Code session). Do not print or paste the credential into chat, logs, or command-line arguments. If the client cannot reach this computer's loopback interface or update its configuration, explain the blocker; do not create a tunnel.
3. Discover the server's tools and call `get_runtime_info` to verify the connection. Do not write to a device just to test it.

## Work with a device

- Start with `scan_devices`. Use the current ID, rescan after any reconnect, and do not choose between multiple devices on the user's behalf. In Maskrom, do not infer the chip from the USB PID. In Loader, check the reported chip and capacity.
- Inspect a firmware with `inspect_firmware`, or use `inspect_archive` to pick the exact archive member. Verify board and storage compatibility before requesting a write. For partition operations use the exact name, start, and length from `read_partitions`; re-read after a storage switch. Sectors are 512 bytes.
- For an authorized mutation, summarize the target and impact and submit once. Wait for approval if the desktop prompts. Flash jobs (including a requested post-flash reboot) can start without a prompt only if the user enabled automatic flash in desktop settings or started the CLI server with `--allow-flash`. The headless server refuses other operations that require approval. Poll `get_operation` with the returned job ID. Cancellation does not undo written sectors. Report failed or cancelled writes before attempting further operations.
