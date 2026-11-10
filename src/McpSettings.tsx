import { useEffect, useState } from "react";
import { Copy, RefreshCw } from "lucide-react";
import { api, native } from "./bridge";
import type { McpFlashApproval, McpStatus } from "./types";
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

export function McpSettings() {
  const [status, setStatus] = useState<McpStatus | null>(null);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copyState, setCopyState] = useState<"idle" | "copied" | "manual">("idle");
  const setup = status?.running && status.configPath
    ? `${skill.trim()}\n\n## Local connection\n\n- Server URL: ${status.endpoint}\n- Generated MCP configuration: ${JSON.stringify(status.configPath)}\n`
    : null;

  useEffect(() => {
    if (!native) return;
    let mounted = true;
    void api.mcpStatus().then(
      (value) => { if (mounted) setStatus(value); },
      (reason) => { if (mounted) setError(String(reason)); },
    );
    return () => {
      mounted = false;
    };
  }, []);

  const loadStatus = async () => {
    setWorking(true);
    setError(null);
    try {
      setStatus(await api.mcpStatus());
    } catch (reason) {
      setError(String(reason));
    } finally {
      setWorking(false);
    }
  };

  const update = async (enabled: boolean) => {
    setWorking(true);
    setError(null);
    setCopyState("idle");
    try {
      setStatus(await api.setMcpEnabled(enabled));
    } catch (reason) {
      setError(String(reason));
    } finally {
      setWorking(false);
    }
  };

  const updateFlashApproval = async (flashApproval: McpFlashApproval) => {
    setWorking(true);
    setError(null);
    try {
      setStatus(await api.setMcpFlashApproval(flashApproval));
    } catch (reason) {
      setError(String(reason));
    } finally {
      setWorking(false);
    }
  };

  const resetToken = async () => {
    setWorking(true);
    setError(null);
    try {
      setStatus(await api.resetMcpToken());
    } catch (reason) {
      setError(String(reason));
    } finally {
      setWorking(false);
    }
  };

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
              checked={status?.enabled ?? false}
              disabled={working || status === null}
              onChange={(event) => void update(event.target.checked)}
            />
            <strong>启用本机 MCP 服务</strong>
          </label>
          {status?.running && setup && (
            <>
              <div className="mcp-connection">
                <code title="本机服务地址">{status.endpoint}</code>
                <button className="button secondary" disabled={working} onClick={async () => {
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
          <label className="settings-checkbox">
            <input
              type="checkbox"
              checked={status?.flashApproval === "automatic"}
              disabled={working || status === null}
              onChange={(event) => void updateFlashApproval(event.target.checked ? "automatic" : "required")}
              aria-describedby="mcp-flash-approval-help mcp-flash-approval-warning"
            />
            <span>
              <strong>允许自动烧录（无需批准）</strong>
              <small id="mcp-flash-approval-help">默认每次烧录都需批准。开启后，持有本机 MCP 密钥的客户端可直接覆盖设备数据。</small>
              <small id="mcp-flash-approval-warning">无人值守烧录前请先备份，核对设备与固件。误写或中断可能导致设备无法启动。</small>
            </span>
          </label>
          {status?.hasToken && (
            <details className="mcp-advanced">
              <summary>高级设置</summary>
              <button className="button secondary" disabled={working || status.enabled} onClick={() => void resetToken()}>
                <RefreshCw size={14} /> 重置密钥
              </button>
              <p>{status.enabled ? "关闭服务后可以重置密钥。" : "重置后，原客户端需重新接入。"}</p>
            </details>
          )}
          {status?.enabled && !status.running && (
            <button className="button secondary" disabled={working} onClick={() => void update(true)}>
              <RefreshCw size={15} /> 重试启动
            </button>
          )}
          {!status && (error || working) && (
            <button className="button secondary" disabled={working} onClick={() => void loadStatus()}>
              <RefreshCw size={15} /> 重新读取状态
            </button>
          )}
          {(error || status?.error) && <p role="alert">{error || status?.error}</p>}
        </>
      )}
    </section>
  );
}
