mod tools;

use crate::{host::Host, state::Backend};
use axum::{
    Router,
    extract::{Request, State as HttpState},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    net::{Ipv4Addr, SocketAddrV4},
    path::{Path, PathBuf},
    sync::{Arc, Mutex as CommitLock, MutexGuard},
    time::Duration,
};
use subtle::ConstantTimeEq;
#[cfg(feature = "desktop")]
use tauri::{AppHandle, State};
#[cfg(feature = "desktop")]
use tokio::sync::Mutex;
use tokio::{net::TcpListener, sync::Semaphore};
use tokio_util::sync::CancellationToken;

const PORT: u16 = 38471;
const HOST: &str = "127.0.0.1:38471";
const ORIGIN: &str = "http://127.0.0.1:38471";
const ENDPOINT: &str = "http://127.0.0.1:38471/mcp";
const CLIENT_CONFIG_NAME: &str = "rkdevtool-next.mcp.json";
#[cfg(feature = "headless-cli")]
pub(crate) const UNATTENDED_FLASH_WARNING: &str = "Back up data and verify the target device and firmware before unattended flashing. An incorrect or interrupted write can leave the device unbootable.";

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum FlashApproval {
    #[default]
    Required,
    Automatic,
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct Settings {
    enabled: bool,
    token: Option<String>,
    #[serde(default)]
    flash_approval: FlashApproval,
}

#[cfg(feature = "desktop")]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Status {
    enabled: bool,
    running: bool,
    endpoint: &'static str,
    config_path: Option<PathBuf>,
    has_token: bool,
    flash_approval: FlashApproval,
    error: Option<String>,
}

#[cfg(feature = "desktop")]
pub(crate) struct Controller {
    path: PathBuf,
    config_path: PathBuf,
    prompt: Arc<Semaphore>,
    inner: Mutex<Inner>,
}

#[cfg(feature = "desktop")]
struct Inner {
    settings: Settings,
    server: Option<Running>,
    error: Option<String>,
}

#[cfg(feature = "desktop")]
impl Inner {
    fn running(&self) -> bool {
        self.server
            .as_ref()
            .is_some_and(|server| !server.task.is_finished())
    }

    fn status(&self, config_path: &Path) -> Status {
        let running = self.running();
        Status {
            enabled: self.settings.enabled,
            running,
            endpoint: ENDPOINT,
            config_path: running.then(|| config_path.to_path_buf()),
            has_token: self.settings.token.is_some(),
            flash_approval: self.settings.flash_approval,
            error: self.error.clone().or_else(|| {
                (self.settings.enabled && !running).then(|| "MCP 服务已停止，请重新开启。".into())
            }),
        }
    }
}

#[cfg(feature = "desktop")]
impl Controller {
    pub async fn restore(path: PathBuf, app: AppHandle, backend: Arc<Backend>) -> Self {
        let config_path = path.with_file_name(CLIENT_CONFIG_NAME);
        let prompt = Arc::new(Semaphore::new(1));
        let (settings, mut error) = match read_settings(&path) {
            Ok(settings) => (settings, None),
            Err(error) => (Settings::default(), Some(error)),
        };
        let server = if settings.enabled {
            let token = settings.token.as_deref().expect("validated settings");
            let listener = async {
                let listener = bind().await?;
                write_client_config(&config_path, token)?;
                Ok(listener)
            }
            .await;
            match listener {
                Ok(listener) => Some(Running::start(
                    listener,
                    token,
                    Host::desktop(app),
                    backend,
                    prompt.clone(),
                    settings.flash_approval,
                )),
                Err(reason) => {
                    error = Some(reason);
                    None
                }
            }
        } else {
            None
        };
        Self {
            path,
            config_path,
            prompt,
            inner: Mutex::new(Inner {
                settings,
                server,
                error,
            }),
        }
    }
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn mcp_status(controller: State<'_, Controller>) -> Result<Status, String> {
    Ok(controller
        .inner
        .lock()
        .await
        .status(&controller.config_path))
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn set_mcp_flash_approval(
    flash_approval: FlashApproval,
    controller: State<'_, Controller>,
) -> Result<Status, String> {
    let mut inner = controller.inner.lock().await;
    if flash_approval != inner.settings.flash_approval {
        let settings = Settings {
            flash_approval,
            ..inner.settings.clone()
        };
        {
            // Change the persisted setting and admission policy together.
            let live_policy = inner
                .server
                .as_ref()
                .map(|server| server.scope.lock_policy())
                .transpose()?;
            write_managed_json(&controller.path, &settings)
                .map_err(|e| format!("无法保存 MCP 烧录授权：{e}"))?;
            if let Some(mut policy) = live_policy {
                *policy = flash_approval;
            }
        }
        inner.settings = settings;
    }
    Ok(inner.status(&controller.config_path))
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn set_mcp_enabled(
    enabled: bool,
    controller: State<'_, Controller>,
    app: AppHandle,
    backend: State<'_, Arc<Backend>>,
) -> Result<Status, String> {
    let mut inner = controller.inner.lock().await;
    if enabled && !inner.running() {
        let listener = bind().await?;
        let token = inner.settings.token.clone().unwrap_or_else(new_token);
        let settings = Settings {
            enabled: true,
            token: Some(token.clone()),
            flash_approval: inner.settings.flash_approval,
        };
        write_client_config(&controller.config_path, &token)?;
        write_managed_json(&controller.path, &settings)
            .map_err(|e| format!("无法保存 MCP 设置：{e}"))?;
        if let Some(old) = inner.server.take() {
            old.stop().await;
        }
        inner.server = Some(Running::start(
            listener,
            &token,
            Host::desktop(app),
            backend.inner().clone(),
            controller.prompt.clone(),
            settings.flash_approval,
        ));
        inner.settings = settings;
    } else if !enabled && inner.settings.enabled {
        let settings = Settings {
            enabled: false,
            ..inner.settings.clone()
        };
        write_managed_json(&controller.path, &settings)
            .map_err(|e| format!("无法保存 MCP 设置：{e}"))?;
        if let Some(server) = inner.server.take() {
            server.stop().await;
        }
        inner.settings = settings;
    } else {
        return Ok(inner.status(&controller.config_path));
    }
    inner.error = None;
    Ok(inner.status(&controller.config_path))
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn reset_mcp_token(controller: State<'_, Controller>) -> Result<Status, String> {
    let mut inner = controller.inner.lock().await;
    if inner.settings.enabled {
        return Err("请先关闭 MCP 服务，再重置密钥。".into());
    }
    let token = new_token();
    let settings = Settings {
        enabled: false,
        token: Some(token.clone()),
        flash_approval: inner.settings.flash_approval,
    };
    // Persist the new key first: after a successful reset, a stale client can
    // never reconnect. Starting the server also regenerates the client config.
    write_managed_json(&controller.path, &settings)
        .map_err(|e| format!("无法重置 MCP 密钥：{e}"))?;
    inner.settings = settings;
    inner.error = None;
    write_client_config(&controller.config_path, &token)?;
    Ok(inner.status(&controller.config_path))
}

fn new_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn write_client_config(path: &Path, token: &str) -> Result<(), String> {
    let config = json!({
        "mcpServers": {
            "rkdevtool-next": {
                "type": "http",
                "url": ENDPOINT,
                "headers": { "Authorization": format!("Bearer {token}") }
            }
        }
    });
    write_managed_json(path, &config).map_err(|e| format!("无法生成 MCP 客户端配置：{e}"))
}

async fn bind() -> Result<TcpListener, String> {
    TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, PORT))
        .await
        .map_err(|e| format!("无法监听 {HOST}：{e}。请释放端口后重新开启 MCP 服务。"))
}

#[derive(Clone)]
struct Scope {
    stopped: CancellationToken,
    flash_approval: Arc<CommitLock<FlashApproval>>,
}

impl Scope {
    fn check(&self) -> Result<(), String> {
        if self.stopped.is_cancelled() {
            Err("MCP 服务已关闭。".into())
        } else {
            Ok(())
        }
    }

    fn lock_policy(&self) -> Result<MutexGuard<'_, FlashApproval>, String> {
        self.flash_approval
            .lock()
            .map_err(|_| "MCP 状态异常，请重启应用。".into())
    }

    fn allows_automatic_flash(&self) -> Result<bool, String> {
        Ok(*self.lock_policy()? == FlashApproval::Automatic)
    }

    fn commit<T>(&self, run: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        self.commit_operation(false, run)
    }

    // Stopping the service or revoking automatic flash takes effect before
    // any subsequent admission; a job already launched is left running.
    fn commit_operation<T>(
        &self,
        automatic_flash: bool,
        run: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let policy = self.lock_policy()?;
        self.check()?;
        if automatic_flash && *policy != FlashApproval::Automatic {
            return Err("自动烧录已关闭，未启动任务。请重新提交并在桌面批准。".into());
        }
        run()
    }

    fn close(&self) {
        let _lock = self
            .flash_approval
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.stopped.cancel();
    }
}

struct Running {
    scope: Scope,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Clone)]
struct Access {
    scope: Scope,
    authorization: Arc<str>,
}

impl Running {
    fn start(
        listener: TcpListener,
        token: &str,
        host: Host,
        backend: Arc<Backend>,
        prompt: Arc<Semaphore>,
        flash_approval: FlashApproval,
    ) -> Self {
        let scope = Scope {
            stopped: CancellationToken::new(),
            flash_approval: Arc::new(CommitLock::new(flash_approval)),
        };
        let handler = tools::Tools::new(host, backend, prompt, scope.clone());
        let config = StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_json_response(true)
            .with_max_request_body_bytes(64 * 1024)
            .with_cancellation_token(scope.stopped.child_token());
        let service = StreamableHttpService::new(
            move || Ok(handler.clone()),
            Arc::new(LocalSessionManager::default()),
            config,
        );
        let router =
            Router::new()
                .route_service("/mcp", service)
                .layer(middleware::from_fn_with_state(
                    Access {
                        scope: scope.clone(),
                        authorization: format!("Bearer {token}").into(),
                    },
                    authorize,
                ));
        let shutdown = scope.stopped.clone();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await;
        });
        Self { scope, task }
    }

    async fn stop(mut self) {
        self.scope.close();
        if tokio::time::timeout(Duration::from_secs(1), &mut self.task)
            .await
            .is_err()
        {
            self.task.abort();
            let _ = self.task.await;
        }
    }
}

#[cfg(feature = "headless-cli")]
pub(crate) async fn serve_headless(directory: PathBuf, allow_flash: bool) -> Result<(), String> {
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|e| format!("Cannot listen for SIGTERM: {e}"))?;
    let shutdown = async {
        #[cfg(unix)]
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
        #[cfg(not(unix))]
        tokio::signal::ctrl_c().await
    };
    // Claim the port before touching the journal or the client configuration.
    let listener = bind().await?;
    let backend = Arc::new(Backend::load(directory.clone())?);
    let token_path = directory.join("mcp-settings.json");
    let config_path = directory.join(CLIENT_CONFIG_NAME);
    let token = match read_settings(&token_path)?.token {
        Some(token) => token,
        None => {
            let token = new_token();
            write_managed_json(
                &token_path,
                &Settings {
                    token: Some(token.clone()),
                    ..Settings::default()
                },
            )?;
            token
        }
    };
    write_client_config(&config_path, &token)?;
    let mut server = Running::start(
        listener,
        &token,
        Host::default(),
        backend.clone(),
        Arc::new(Semaphore::new(1)),
        if allow_flash {
            FlashApproval::Automatic
        } else {
            FlashApproval::Required
        },
    );
    println!("MCP listening at {ENDPOINT}");
    println!("Client configuration: {}", config_path.display());
    println!(
        "Access: {}",
        if allow_flash {
            "automatic flash"
        } else {
            "read-only"
        }
    );
    if allow_flash {
        eprintln!("Warning: {UNATTENDED_FLASH_WARNING}");
    }
    let signal = tokio::select! {
        result = shutdown => Some(result),
        _ = &mut server.task => None,
    };
    let result = if let Some(result) = signal {
        server.stop().await;
        result.map_err(|e| format!("Shutdown signal failed: {e}"))
    } else {
        server.scope.close();
        Err("MCP listener stopped unexpectedly".into())
    };
    if backend.is_busy() {
        eprintln!("Waiting for the running flash to finish before exiting.");
        while backend.is_busy() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    result
}

fn single_header(headers: &HeaderMap, name: header::HeaderName, expected: &[u8]) -> bool {
    let values = headers.get_all(name);
    let mut values = values.iter();
    values
        .next()
        .is_some_and(|value| bool::from(value.as_bytes().ct_eq(expected)))
        && values.next().is_none()
}

async fn authorize(
    HttpState(access): HttpState<Access>,
    mut request: Request,
    next: Next,
) -> Response {
    if access.scope.check().is_err() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let headers = request.headers();
    if !single_header(headers, header::HOST, HOST.as_bytes())
        || (headers.contains_key(header::ORIGIN)
            && !single_header(headers, header::ORIGIN, ORIGIN.as_bytes()))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !single_header(
        headers,
        header::AUTHORIZATION,
        access.authorization.as_bytes(),
    ) {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
        )
            .into_response();
    }
    request.headers_mut().remove(header::AUTHORIZATION);
    next.run(request).await
}

fn read_settings(path: &Path) -> Result<Settings, String> {
    let contents = match fs::read(path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Settings::default()),
        Err(e) => return Err(format!("无法读取 MCP 设置：{e}")),
    };
    let settings: Settings = serde_json::from_slice(&contents)
        .map_err(|e| format!("MCP 设置无法读取：{e}。重新开启可生成新的连接配置。"))?;
    if (settings.enabled || settings.token.is_some())
        && !settings
            .token
            .as_deref()
            .is_some_and(|token| token.len() == 32 && token.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err("MCP 连接凭据无效。重新开启可生成新的连接配置。".into());
    }
    Ok(settings)
}

fn write_managed_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let contents = json_bytes(value)?;
    let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4().simple()));
    write_new_file(&temp, &contents)?;
    fs::rename(&temp, path).map_err(|e| {
        let _ = fs::remove_file(&temp);
        format!("无法替换本地文件：{e}")
    })
}

fn json_bytes(value: &impl Serialize) -> Result<Vec<u8>, String> {
    let mut contents = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    contents.push(b'\n');
    Ok(contents)
}

fn write_new_file(path: &Path, contents: &[u8]) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            "文件已存在，请选择新文件名；原文件未被覆盖。".into()
        } else {
            format!("无法创建文件：{e}")
        }
    })?;
    let result = file.write_all(contents).and_then(|_| file.sync_all());
    drop(file);
    result.map_err(|e| format!("文件保存失败，目标可能只写入部分内容：{e}"))
}
