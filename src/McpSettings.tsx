import { useState } from "react";
import { Copy, RefreshCw } from "lucide-react";
import { native } from "./bridge";
import type { McpFlashApproval, McpSettingsDraft, McpStatus } from "./types";
import skill from "../skills/rkdevtool-next/SKILL.md?raw";

async function copyText(text: string): Promise<boolean> {
  try {
    if (navigator.clipboard) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    // Some native webviews do not grant Clipboard API access to their origin.
  }
  const field = document.createElement("textarea");
  field.value = text;
  field.readOnly = true;
  field.style.cssText = "position:fixed;top:0;left:0;opacity:0";
  // Keep the temporary selection inside the modal so it can receive focus.
  (document.querySelector("dialog[open]") ?? document.body).append(field);
  const focused = document.activeElement;
  field.select();
  try {
    return document.execCommand("copy");
  } catch {
    return false;
  } finally {
    field.remove();
    if (focused instanceof HTMLElement) focused.focus();
  }
}

export function McpSettings({ status, draft, saving, loadError, reload, change }: {
  status: McpStatus | null;
  draft: McpSettingsDraft | null;
  saving: boolean;
  loadError: string | null;
  reload: () => void;
  change: (draft: McpSettingsDraft) => void;
}) {
  const [copyState, setCopyState] = useState<"idle" | "copied" | "manual">("idle");
  const setup = status?.running && status.configPath
    ? `${skill.trim()}\n\n## Local connection\n\n- Server URL: ${status.endpoint}\n- Generated MCP configuration: ${JSON.stringify(status.configPath)}\n`
    : null;

  return (
    <section className="settings-group mcp-settings" aria-labelledby="mcp-settings-title">
      <h3 id="mcp-settings-title">MCP 服务器</h3>
      {!native ? (
        <p>仅在 RKDevTool Next 桌面版可用。</p>
      ) : (
        <>
          <label className="settings-checkbox">
            <input
              type="checkbox"
              checked={draft?.enabled ?? false}
              disabled={saving || draft === null}
              onChange={(event) => {
                if (draft) change({ ...draft, enabled: event.target.checked, resetToken: false });
              }}
            />
            <strong>启用本机 MCP 服务</strong>
          </label>
          {status?.running && setup && (
            <>
              <div className="mcp-connection">
                <code title="本机服务地址">{status.endpoint}</code>
                <button className="button secondary" disabled={saving} onClick={async () => {
                  setCopyState(await copyText(setup) ? "copied" : "manual");
                }}>
                  <Copy size={14} />
                  <span aria-live="polite">{copyState === "copied" ? "已复制" : "复制接入 Skill"}</span>
                </button>
              </div>
              {copyState === "manual" && (
                <label className="mcp-manual-copy">
                  手动复制以下内容，粘贴给 Agent
                  <textarea readOnly rows={4} value={setup} onFocus={(event) => event.target.select()} />
                </label>
              )}
            </>
          )}
          <label className="mcp-approval">
            <strong>MCP 授权模式</strong>
            <select
              value={draft?.flashApproval ?? "required"}
              disabled={saving || draft === null}
              onChange={(event) => {
                if (draft) change({ ...draft, flashApproval: event.target.value as McpFlashApproval });
              }}
              aria-describedby="mcp-approval-warning"
            >
              <option value="required">逐次批准（默认）</option>
              <option value="automatic">仅烧录免批准</option>
              <option value="unattended">全权限无人值守（高风险）</option>
            </select>
          </label>
          <p id="mcp-approval-warning" className="mcp-approval-warning">
            <strong>高风险：</strong>全权限无人值守允许持有本机 MCP 密钥的客户端不经确认地擦除、烧录、加载 Loader、重启、备份及导出文件。误操作可能永久丢失数据或使设备无法启动。只向可信客户端提供配置，先备份，用完切回逐次批准。
          </p>
          {status?.hasToken && (
            <details className="mcp-advanced">
              <summary>高级设置</summary>
              <label className="settings-checkbox">
                <input
                  type="checkbox"
                  checked={draft?.resetToken ?? false}
                  disabled={saving || !draft || status.enabled || draft.enabled}
                  onChange={(event) => {
                    if (draft) change({ ...draft, resetToken: event.target.checked });
                  }}
                />
                <span>
                  <strong>保存时重置 MCP 密钥</strong>
                  <small>{status.enabled ? "先保存关闭服务的设置。" : "重置后，原客户端需重新接入。"}</small>
                </span>
              </label>
            </details>
          )}
          {status?.enabled && !status.running && draft?.enabled && (
            <p>点击「保存」可重试启动 MCP 服务。</p>
          )}
          {!status && loadError && (
            <button className="button secondary" disabled={saving} onClick={reload}>
              <RefreshCw size={15} /> 重新读取状态
            </button>
          )}
          {(loadError || status?.error) && <p role="alert">{loadError || status?.error}</p>}
        </>
      )}
    </section>
  );
}
