use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use neo_core::{
    Allowance, ModelCapabilities, ModelInfo, ModelRef, ModelUseCase, ProviderAccount,
    ProviderAccountStatus, ProviderId, RateLimitKind, RateLimitWindow,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, broadcast, oneshot};
use tokio::time::{Duration, Instant, timeout, timeout_at};
use url::Url;

const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const NOTIFICATION_CAPACITY: usize = 128;
const PROVIDER_ID: &str = "chatgpt-codex";
/// JSON-RPC's own "this method is not served". The app-server may ask the
/// client for something; Neo answers none of it, and says so this way.
const JSONRPC_METHOD_NOT_FOUND: i64 = -32_601;

type BoxWriter = Pin<Box<dyn AsyncWrite + Send>>;
type PendingResult = std::result::Result<Value, RpcFailure>;

#[derive(Debug, thiserror::Error)]
pub enum CodexError {
    #[error("Codex app-server I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Codex app-server JSON was invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Codex app-server URL was invalid: {0}")]
    Url(#[from] url::ParseError),
    #[error("Codex app-server protocol error: {0}")]
    Protocol(String),
    #[error("Codex app-server RPC {code}: {message}")]
    Rpc { code: i64, message: String },
    #[error("Codex app-server request `{method}` timed out")]
    Timeout { method: String },
    /// The app-server is gone, and why. The reason is the read loop's last
    /// word — a malformed line, a closed pipe, a read failure — and carrying
    /// it is the difference between "exited" and a diagnosis.
    #[error("Codex app-server exited: {0}")]
    Exited(String),
    #[error("system clock failed: {0}")]
    Clock(#[from] std::time::SystemTimeError),
    #[error("system clock cannot fit in milliseconds")]
    ClockOverflow,
    #[error("ChatGPT login failed: {0}")]
    Login(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexAccount {
    pub status: ProviderAccountStatus,
    pub email: Option<String>,
    pub plan_type: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexLoginStart {
    pub login_id: String,
    pub auth_url: Url,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexLoginCompletion {
    pub login_id: Option<String>,
    pub success: bool,
    pub error: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CodexTextTurn {
    pub thread_id: String,
    pub turn_id: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CodexNotification {
    LoginCompleted(CodexLoginCompletion),
    AccountUpdated {
        auth_mode: Option<String>,
        plan_type: Option<String>,
    },
    RateLimitsUpdated(Allowance),
    AgentMessage {
        thread_id: String,
        turn_id: String,
        text: String,
    },
    TurnCompleted {
        thread_id: String,
        turn_id: String,
        status: String,
        error: Option<String>,
    },
    ForbiddenItem {
        thread_id: String,
        turn_id: String,
        kind: String,
    },
    Other {
        method: String,
    },
}

pub struct CodexLoginAttempt {
    start: CodexLoginStart,
    notifications: broadcast::Receiver<CodexNotification>,
}

impl CodexLoginAttempt {
    pub fn start(&self) -> &CodexLoginStart {
        &self.start
    }

    pub async fn wait(mut self, wait_for: Duration) -> Result<CodexLoginCompletion, CodexError> {
        let deadline = Instant::now() + wait_for;
        loop {
            let notification = timeout_at(deadline, self.notifications.recv())
                .await
                .map_err(|_| CodexError::Timeout {
                    method: "account/login/completed".into(),
                })?;
            match notification {
                Ok(CodexNotification::LoginCompleted(completion))
                    if completion.login_id.as_deref() == Some(self.start.login_id.as_str()) =>
                {
                    if completion.success {
                        return Ok(completion);
                    }
                    return Err(CodexError::Login(
                        completion.error.unwrap_or_else(|| "unknown error".into()),
                    ));
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(CodexError::Exited(
                        "the notification stream closed while waiting for the login".to_owned(),
                    ));
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct CodexClient {
    inner: Arc<ClientInner>,
}

struct ClientInner {
    writer: Mutex<BoxWriter>,
    pending: std::sync::Mutex<HashMap<u64, oneshot::Sender<PendingResult>>>,
    notifications: broadcast::Sender<CodexNotification>,
    next_id: AtomicU64,
    request_timeout: Duration,
    /// Why the read loop stopped, once it has.
    ///
    /// **This is what a later request checks.** `fail_pending` answered the
    /// requests that were already waiting and nothing else, so the first
    /// request issued after the loop died minted an id, inserted itself into
    /// `pending`, wrote into a pipe nobody reads and then waited out the
    /// whole `request_timeout` before reporting `Timeout` — the wrong
    /// reason, once per request, for the rest of the process's life.
    /// `CodexSupervisor::has_exited` existed for exactly this and was called
    /// by nothing.
    ///
    /// Written and read under the `pending` lock, so a request cannot pass
    /// the check and then insert itself into a map that has just been
    /// drained for the last time.
    closed: std::sync::OnceLock<String>,
}

#[derive(Debug)]
struct RpcFailure {
    code: i64,
    message: String,
}

impl CodexClient {
    pub async fn connect<R, W>(
        reader: R,
        writer: W,
        request_timeout: Duration,
    ) -> Result<Self, CodexError>
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + 'static,
    {
        let (notifications, _) = broadcast::channel(NOTIFICATION_CAPACITY);
        let inner = Arc::new(ClientInner {
            writer: Mutex::new(Box::pin(writer)),
            pending: std::sync::Mutex::new(HashMap::new()),
            notifications,
            next_id: AtomicU64::new(1),
            request_timeout,
            closed: std::sync::OnceLock::new(),
        });
        tokio::spawn(read_loop(reader, Arc::clone(&inner)));
        let client = Self { inner };
        client.initialize().await?;
        Ok(client)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CodexNotification> {
        self.inner.notifications.subscribe()
    }

    pub async fn read_account(&self, refresh_token: bool) -> Result<CodexAccount, CodexError> {
        let result = self
            .request("account/read", json!({ "refreshToken": refresh_token }))
            .await?;
        let result: AccountReadResult = serde_json::from_value(result)?;
        Ok(match result.account {
            None => CodexAccount {
                status: ProviderAccountStatus::SignedOut,
                email: None,
                plan_type: None,
            },
            Some(AccountWire::ChatGpt { email, plan_type }) => CodexAccount {
                status: ProviderAccountStatus::Connected,
                email,
                plan_type,
            },
            Some(AccountWire::Other) => CodexAccount {
                status: ProviderAccountStatus::Unavailable,
                email: None,
                plan_type: None,
            },
        })
    }

    pub async fn read_allowance(&self) -> Result<Allowance, CodexError> {
        let result = self.request("account/rateLimits/read", json!({})).await?;
        parse_allowance(&result)
    }

    pub async fn read_provider_account(&self) -> Result<ProviderAccount, CodexError> {
        let mut account = self.read_account(false).await?;
        let allowance = if account.status == ProviderAccountStatus::Connected {
            let allowance = self.read_allowance().await?;
            if allowance
                .limits
                .iter()
                .any(|window| window.used_percent >= 100.0)
            {
                account.status = ProviderAccountStatus::RateLimited;
            }
            Some(allowance)
        } else {
            None
        };
        Ok(ProviderAccount {
            provider: ProviderId::new(PROVIDER_ID),
            status: account.status,
            email: account.email,
            plan_type: account.plan_type,
            workspace: None,
            allowance,
            updated_at: now_ms()?,
        })
    }

    pub async fn start_chatgpt_login(&self) -> Result<CodexLoginAttempt, CodexError> {
        let notifications = self.subscribe();
        let result = self
            .request(
                "account/login/start",
                json!({
                    "type": "chatgpt",
                    "useHostedLoginSuccessPage": true,
                    "appBrand": "chatgpt"
                }),
            )
            .await?;
        let result: LoginStartResult = serde_json::from_value(result)?;
        if result.kind != "chatgpt" {
            return Err(CodexError::Protocol(format!(
                "unexpected login type `{}`",
                result.kind
            )));
        }
        Ok(CodexLoginAttempt {
            start: CodexLoginStart {
                login_id: result.login_id,
                auth_url: Url::parse(&result.auth_url)?,
            },
            notifications,
        })
    }

    pub async fn cancel_login(&self, login_id: &str) -> Result<(), CodexError> {
        self.request("account/login/cancel", json!({ "loginId": login_id }))
            .await?;
        Ok(())
    }

    pub async fn logout(&self) -> Result<(), CodexError> {
        self.request("account/logout", json!({})).await?;
        Ok(())
    }

    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, CodexError> {
        let mut cursor: Option<String> = None;
        let mut models = Vec::new();
        loop {
            let result = self
                .request(
                    "model/list",
                    json!({
                        "cursor": cursor,
                        "limit": 100,
                        "includeHidden": false
                    }),
                )
                .await?;
            let page: ModelPage = serde_json::from_value(result)?;
            models.extend(page.data.into_iter().map(model_info));
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        Ok(models)
    }

    pub async fn complete_text(
        &self,
        prompt: &str,
        model: Option<&str>,
        wait_for: Duration,
    ) -> Result<CodexTextTurn, CodexError> {
        if prompt.is_empty() || prompt.len() > 64 * 1024 {
            return Err(CodexError::Protocol(
                "text prompt must contain 1 to 65536 bytes".into(),
            ));
        }
        let thread = self
            .request(
                "thread/start",
                json!({
                    "approvalPolicy": "never",
                    "baseInstructions": "Answer the user's request using only your own reasoning. Do not run commands, read or write files, browse, use MCP, or call tools.",
                    "ephemeral": true,
                    "model": model,
                    "sandbox": "read-only"
                }),
            )
            .await?;
        let thread_id = required_string(&thread, "/thread/id", "thread/start thread id")?;
        let mut notifications = self.subscribe();
        let turn = self
            .request(
                "turn/start",
                json!({
                    "threadId": thread_id,
                    "input": [{ "type": "text", "text": prompt }]
                }),
            )
            .await?;
        let turn_id = required_string(&turn, "/turn/id", "turn/start turn id")?;
        let deadline = Instant::now() + wait_for;
        let mut answer = String::new();
        loop {
            let notification = timeout_at(deadline, notifications.recv())
                .await
                .map_err(|_| CodexError::Timeout {
                    method: "turn/completed".into(),
                })?;
            match notification {
                Ok(CodexNotification::AgentMessage {
                    thread_id: event_thread,
                    turn_id: event_turn,
                    text,
                }) if event_thread == thread_id && event_turn == turn_id => answer = text,
                Ok(CodexNotification::TurnCompleted {
                    thread_id: event_thread,
                    turn_id: event_turn,
                    status,
                    error,
                }) if event_thread == thread_id && event_turn == turn_id => {
                    if status != "completed" {
                        return Err(CodexError::Protocol(error.unwrap_or_else(|| {
                            format!("Codex turn ended with status `{status}`")
                        })));
                    }
                    if answer.is_empty() {
                        return Err(CodexError::Protocol(
                            "Codex turn completed without an agent message".into(),
                        ));
                    }
                    return Ok(CodexTextTurn {
                        thread_id,
                        turn_id,
                        text: answer,
                    });
                }
                Ok(CodexNotification::ForbiddenItem {
                    thread_id: event_thread,
                    turn_id: event_turn,
                    kind,
                }) if event_thread == thread_id && event_turn == turn_id => {
                    return Err(CodexError::Protocol(format!(
                        "text-only Codex turn attempted forbidden item `{kind}`"
                    )));
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    return Err(CodexError::Protocol(format!(
                        "missed {skipped} Codex notifications"
                    )));
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(CodexError::Exited(
                        "the notification stream closed mid-turn".to_owned(),
                    ));
                }
            }
        }
    }

    async fn initialize(&self) -> Result<(), CodexError> {
        self.request(
            "initialize",
            json!({
                "clientInfo": {
                    "name": "starkbot_neo",
                    "title": "Starkbot Neo",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": { "experimentalApi": true }
            }),
        )
        .await?;
        self.notify("initialized", json!({})).await
    }

    /// One request, answered by the read loop or refused at once.
    ///
    /// # Errors
    ///
    /// [`CodexError::Exited`] when the read loop has already stopped — which
    /// is checked *before* an id is minted, because every request after that
    /// point otherwise waited out the full `request_timeout` to report a
    /// timeout that was really an exit. See [`ClientInner::closed`].
    async fn request(&self, method: &str, params: Value) -> Result<Value, CodexError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        {
            // The latch is read under the `pending` lock that `close` sets it
            // under. Without that ordering a request could pass the check,
            // `close` could drain, and the request would still end up in a
            // map nothing reads again — the same stall, just narrower.
            let mut pending = lock_pending(&self.inner)?;
            if let Some(reason) = self.inner.closed.get() {
                return Err(CodexError::Exited(reason.clone()));
            }
            pending.insert(id, sender);
        }
        if let Err(error) = self
            .write_message(&json!({ "method": method, "id": id, "params": params }))
            .await
        {
            lock_pending(&self.inner)?.remove(&id);
            return Err(error);
        }
        let response = match timeout(self.inner.request_timeout, receiver).await {
            Ok(response) => response.map_err(|_| {
                CodexError::Exited(
                    self.inner
                        .closed
                        .get()
                        .cloned()
                        .unwrap_or_else(|| "the request was dropped unanswered".to_owned()),
                )
            })?,
            Err(_) => {
                lock_pending(&self.inner)?.remove(&id);
                return Err(CodexError::Timeout {
                    method: method.to_owned(),
                });
            }
        };
        match response {
            Ok(value) => Ok(value),
            Err(failure) => Err(CodexError::Rpc {
                code: failure.code,
                message: failure.message,
            }),
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), CodexError> {
        self.write_message(&json!({ "method": method, "params": params }))
            .await
    }

    async fn write_message(&self, message: &Value) -> Result<(), CodexError> {
        send_message(&self.inner, message).await
    }
}

async fn read_loop<R>(reader: R, inner: Arc<ClientInner>)
where
    R: AsyncRead + Send + Unpin + 'static,
{
    let mut reader = BufReader::new(reader);
    let failure = loop {
        let mut encoded = Vec::new();
        match reader.read_until(b'\n', &mut encoded).await {
            Ok(0) => break "app-server closed stdout".to_owned(),
            Ok(_) if encoded.len() > MAX_MESSAGE_BYTES => {
                break "inbound message exceeds 1 MiB".to_owned();
            }
            Ok(_) => {
                while matches!(encoded.last(), Some(b'\n' | b'\r')) {
                    encoded.pop();
                }
                if encoded.is_empty() {
                    continue;
                }
                let message = match serde_json::from_slice::<Value>(&encoded) {
                    Ok(message) => message,
                    Err(error) => break format!("invalid JSON: {error}"),
                };
                if let Err(error) = dispatch_message(message, &inner).await {
                    break error;
                }
            }
            Err(error) => break format!("read failed: {error}"),
        }
    };
    close(&inner, failure);
}

/// One line off the app-server, to whoever was waiting for it.
///
/// `Err` ends the read loop, so only a failure that makes the connection
/// unusable belongs there. A message this client does not implement is not
/// one of those — see [`refuse_request`].
async fn dispatch_message(message: Value, inner: &ClientInner) -> Result<(), String> {
    if let Some(id) = message.get("id").and_then(Value::as_u64) {
        if let Some(method) = message.get("method").and_then(Value::as_str) {
            return refuse_request(id, method, inner).await;
        }
        let sender = lock_pending(inner)
            .map_err(|error| error.to_string())?
            .remove(&id);
        let Some(sender) = sender else {
            return Ok(());
        };
        let result = if let Some(error) = message.get("error") {
            let code = error.get("code").and_then(Value::as_i64).unwrap_or(-32_000);
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown RPC error")
                .to_owned();
            Err(RpcFailure { code, message })
        } else {
            Ok(message.get("result").cloned().unwrap_or(Value::Null))
        };
        let _ = sender.send(result);
        return Ok(());
    }

    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return Err("message has neither an id nor a method".into());
    };
    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
    let notification = parse_notification(method, params).map_err(|error| error.to_string())?;
    let _ = inner.notifications.send(notification);
    Ok(())
}

/// Answer a server-initiated request with "method not found".
///
/// The app-server may ask its client for something — a permission, a piece
/// of configuration. Neo serves none of it, and JSON-RPC already has a way
/// to say so. Returning `Err` instead ended the read loop, and with it every
/// request issued for the rest of the process's life, over one message that
/// is merely unimplemented. Only a write failure ends the loop from here:
/// the pipe really is gone then.
async fn refuse_request(id: u64, method: &str, inner: &ClientInner) -> Result<(), String> {
    let response = json!({
        "id": id,
        "error": {
            "code": JSONRPC_METHOD_NOT_FOUND,
            "message": format!("`{method}` is not served: this client answers no requests"),
        }
    });
    send_message(inner, &response)
        .await
        .map_err(|error| format!("could not refuse `{method}`: {error}"))
}

/// One line to the app-server. A free function because the read loop writes
/// too, and it holds an `Arc<ClientInner>` rather than a [`CodexClient`].
async fn send_message(inner: &ClientInner, message: &Value) -> Result<(), CodexError> {
    let mut encoded = serde_json::to_vec(message)?;
    if encoded.len() > MAX_MESSAGE_BYTES {
        return Err(CodexError::Protocol(
            "outbound message exceeds 1 MiB".into(),
        ));
    }
    encoded.push(b'\n');
    let mut writer = inner.writer.lock().await;
    writer.write_all(&encoded).await?;
    writer.flush().await?;
    Ok(())
}

fn parse_notification(method: &str, params: Value) -> Result<CodexNotification, CodexError> {
    match method {
        "account/login/completed" => {
            let params: LoginCompletedParams = serde_json::from_value(params)?;
            Ok(CodexNotification::LoginCompleted(CodexLoginCompletion {
                login_id: params.login_id,
                success: params.success,
                error: params.error,
            }))
        }
        "account/updated" => {
            let params: AccountUpdatedParams = serde_json::from_value(params)?;
            Ok(CodexNotification::AccountUpdated {
                auth_mode: params.auth_mode,
                plan_type: params.plan_type,
            })
        }
        "account/rateLimits/updated" => Ok(CodexNotification::RateLimitsUpdated(parse_allowance(
            &params,
        )?)),
        "item/completed" => parse_completed_item(params),
        "turn/completed" => {
            let thread_id = required_string(&params, "/threadId", "completed turn thread id")?;
            let turn_id = required_string(&params, "/turn/id", "completed turn id")?;
            if let Some(kind) = params
                .pointer("/turn/items")
                .and_then(Value::as_array)
                .and_then(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.get("type").and_then(Value::as_str))
                        .find(|kind| is_forbidden_item(kind))
                })
            {
                return Ok(CodexNotification::ForbiddenItem {
                    thread_id,
                    turn_id,
                    kind: kind.to_owned(),
                });
            }
            let status = required_string(&params, "/turn/status", "completed turn status")?;
            let error = params
                .pointer("/turn/error/message")
                .and_then(Value::as_str)
                .map(str::to_owned);
            Ok(CodexNotification::TurnCompleted {
                thread_id,
                turn_id,
                status,
                error,
            })
        }
        _ => Ok(CodexNotification::Other {
            method: method.to_owned(),
        }),
    }
}

fn parse_completed_item(params: Value) -> Result<CodexNotification, CodexError> {
    let thread_id = required_string(&params, "/threadId", "completed item thread id")?;
    let turn_id = required_string(&params, "/turnId", "completed item turn id")?;
    let kind = required_string(&params, "/item/type", "completed item type")?;
    if kind == "agentMessage" {
        let text = required_string(&params, "/item/text", "agent message text")?;
        return Ok(CodexNotification::AgentMessage {
            thread_id,
            turn_id,
            text,
        });
    }
    if is_forbidden_item(&kind) {
        return Ok(CodexNotification::ForbiddenItem {
            thread_id,
            turn_id,
            kind,
        });
    }
    Ok(CodexNotification::Other {
        method: format!("item/completed:{kind}"),
    })
}

fn is_forbidden_item(kind: &str) -> bool {
    matches!(
        kind,
        "commandExecution"
            | "fileChange"
            | "mcpToolCall"
            | "dynamicToolCall"
            | "webSearch"
            | "imageGeneration"
    )
}

fn required_string(value: &Value, pointer: &str, label: &str) -> Result<String, CodexError> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| CodexError::Protocol(format!("{label} is missing")))
}

fn parse_allowance(value: &Value) -> Result<Allowance, CodexError> {
    let response: RateLimitsResponse = serde_json::from_value(value.clone())?;
    let mut limits = Vec::new();
    if response.rate_limits_by_limit_id.is_empty() {
        if let Some(limit) = response.rate_limits {
            append_limit_windows(&mut limits, limit)?;
        }
    } else {
        for limit in response.rate_limits_by_limit_id.into_values() {
            append_limit_windows(&mut limits, limit)?;
        }
    }
    Ok(Allowance { limits })
}

fn append_limit_windows(
    output: &mut Vec<RateLimitWindow>,
    limit: RateLimitWire,
) -> Result<(), CodexError> {
    if let Some(primary) = limit.primary {
        output.push(rate_limit_window(
            &limit.limit_id,
            limit.limit_name.as_deref(),
            RateLimitKind::Primary,
            primary,
        )?);
    }
    if let Some(secondary) = limit.secondary {
        output.push(rate_limit_window(
            &limit.limit_id,
            limit.limit_name.as_deref(),
            RateLimitKind::Secondary,
            secondary,
        )?);
    }
    Ok(())
}

fn rate_limit_window(
    limit_id: &str,
    limit_name: Option<&str>,
    kind: RateLimitKind,
    window: RateLimitWindowWire,
) -> Result<RateLimitWindow, CodexError> {
    let resets_at = window
        .resets_at
        .map(|seconds| {
            seconds.checked_mul(1_000).ok_or_else(|| {
                CodexError::Protocol("rate-limit reset overflows milliseconds".into())
            })
        })
        .transpose()?;
    Ok(RateLimitWindow {
        limit_id: limit_id.to_owned(),
        limit_name: limit_name.map(str::to_owned),
        used_percent: window.used_percent,
        kind,
        window_duration_minutes: window.window_duration_mins,
        resets_at,
    })
}

fn model_info(model: ModelWire) -> ModelInfo {
    let modalities = model
        .input_modalities
        .unwrap_or_else(|| vec!["text".into(), "image".into()]);
    ModelInfo {
        reference: ModelRef::new(ProviderId::new(PROVIDER_ID), model.id),
        use_cases: vec![ModelUseCase::Inference, ModelUseCase::TextHelper],
        capabilities: ModelCapabilities {
            reasoning: !model.supported_reasoning_efforts.is_empty(),
            tools: true,
            image_input: modalities.iter().any(|item| item == "image"),
            streaming: true,
        },
        price: None,
        deprecated: model.hidden,
    }
}

fn lock_pending(
    inner: &ClientInner,
) -> Result<std::sync::MutexGuard<'_, HashMap<u64, oneshot::Sender<PendingResult>>>, CodexError> {
    inner
        .pending
        .lock()
        .map_err(|_| CodexError::Protocol("pending-request lock poisoned".into()))
}

/// Latch the connection closed and answer everything still waiting.
///
/// The latch goes up under the same lock the pending map is drained under,
/// so [`CodexClient::request`] can never pass its check and then insert into
/// a map that will not be read again. See [`ClientInner::closed`].
fn close(inner: &ClientInner, reason: String) {
    let Ok(mut pending) = inner.pending.lock() else {
        // Poisoned. The latch is still worth raising: a caller that cannot
        // lock the map is told the lock is poisoned, and one that comes later
        // is told the connection is gone, which is the truth in both cases.
        let _ = inner.closed.set(reason);
        return;
    };
    let _ = inner.closed.set(reason.clone());
    for (_, sender) in pending.drain() {
        let _ = sender.send(Err(RpcFailure {
            code: -32_000,
            message: reason.clone(),
        }));
    }
}

fn now_ms() -> Result<i64, CodexError> {
    let milliseconds = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    i64::try_from(milliseconds).map_err(|_| CodexError::ClockOverflow)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountReadResult {
    account: Option<AccountWire>,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum AccountWire {
    #[serde(rename = "chatgpt")]
    ChatGpt {
        email: Option<String>,
        #[serde(rename = "planType")]
        plan_type: Option<String>,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginStartResult {
    #[serde(rename = "type")]
    kind: String,
    login_id: String,
    auth_url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginCompletedParams {
    login_id: Option<String>,
    success: bool,
    error: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountUpdatedParams {
    auth_mode: Option<String>,
    plan_type: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RateLimitsResponse {
    rate_limits: Option<RateLimitWire>,
    #[serde(default)]
    rate_limits_by_limit_id: HashMap<String, RateLimitWire>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateLimitWire {
    limit_id: String,
    limit_name: Option<String>,
    primary: Option<RateLimitWindowWire>,
    secondary: Option<RateLimitWindowWire>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateLimitWindowWire {
    used_percent: f64,
    window_duration_mins: Option<u64>,
    resets_at: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelPage {
    data: Vec<ModelWire>,
    next_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelWire {
    id: String,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    supported_reasoning_efforts: Vec<Value>,
    input_modalities: Option<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

    #[tokio::test]
    async fn account_allowance_and_models_follow_the_documented_protocol() {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let (reader, mut writer) = tokio::io::split(server_io);
            let mut reader = BufReader::new(reader);
            let initialize = read_message(&mut reader).await;
            assert_eq!(initialize["method"], "initialize");
            assert_eq!(initialize["params"]["clientInfo"]["name"], "starkbot_neo");
            respond(&mut writer, &initialize, json!({ "userAgent": "fixture" })).await;
            assert_eq!(read_message(&mut reader).await["method"], "initialized");

            let account = read_message(&mut reader).await;
            assert_eq!(account["method"], "account/read");
            respond(
                &mut writer,
                &account,
                json!({
                    "account": { "type": "chatgpt", "email": "user@example.com", "planType": "pro" },
                    "requiresOpenaiAuth": true
                }),
            )
            .await;

            let limits = read_message(&mut reader).await;
            assert_eq!(limits["method"], "account/rateLimits/read");
            respond(
                &mut writer,
                &limits,
                json!({
                    "rateLimits": {
                        "limitId": "codex",
                        "limitName": null,
                        "primary": { "usedPercent": 25, "windowDurationMins": 15, "resetsAt": 1730947200 },
                        "secondary": null
                    },
                    "rateLimitsByLimitId": {}
                }),
            )
            .await;

            let models = read_message(&mut reader).await;
            assert_eq!(models["method"], "model/list");
            respond(
                &mut writer,
                &models,
                json!({
                    "data": [{
                        "id": "gpt-5.6-sol",
                        "hidden": false,
                        "supportedReasoningEfforts": [{ "reasoningEffort": "low" }],
                        "inputModalities": ["text", "image"]
                    }],
                    "nextCursor": null
                }),
            )
            .await;
        });

        let (reader, writer) = tokio::io::split(client_io);
        let client = CodexClient::connect(reader, writer, Duration::from_secs(1))
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        let account = client
            .read_provider_account()
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(account.status, ProviderAccountStatus::Connected);
        assert_eq!(account.email.as_deref(), Some("user@example.com"));
        assert_eq!(
            account
                .allowance
                .as_ref()
                .and_then(|allowance| allowance.limits.first())
                .map(|window| window.resets_at),
            Some(Some(1_730_947_200_000))
        );
        let models = client
            .list_models()
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].reference.provider.as_str(), PROVIDER_ID);
        assert!(models[0].capabilities.image_input);
        server.await.unwrap_or_else(|error| panic!("{error}"));
    }

    #[tokio::test]
    async fn login_completion_is_correlated_without_exposing_tokens() {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let (reader, mut writer) = tokio::io::split(server_io);
            let mut reader = BufReader::new(reader);
            let initialize = read_message(&mut reader).await;
            respond(&mut writer, &initialize, json!({})).await;
            let _initialized = read_message(&mut reader).await;
            let login = read_message(&mut reader).await;
            assert_eq!(login["params"]["type"], "chatgpt");
            assert!(login["params"].get("apiKey").is_none());
            respond(
                &mut writer,
                &login,
                json!({
                    "type": "chatgpt",
                    "loginId": "login-1",
                    "authUrl": "https://chatgpt.com/auth"
                }),
            )
            .await;
            send(
                &mut writer,
                json!({
                    "method": "account/login/completed",
                    "params": { "loginId": "login-1", "success": true, "error": null }
                }),
            )
            .await;
        });

        let (reader, writer) = tokio::io::split(client_io);
        let client = CodexClient::connect(reader, writer, Duration::from_secs(1))
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        let attempt = client
            .start_chatgpt_login()
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(attempt.start().login_id, "login-1");
        let completion = attempt
            .wait(Duration::from_secs(1))
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(completion.success);
        server.await.unwrap_or_else(|error| panic!("{error}"));
    }

    #[tokio::test]
    async fn text_turn_is_read_only_and_returns_completed_agent_message() {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let (reader, mut writer) = tokio::io::split(server_io);
            let mut reader = BufReader::new(reader);
            let initialize = read_message(&mut reader).await;
            respond(&mut writer, &initialize, json!({})).await;
            let _initialized = read_message(&mut reader).await;

            let thread = read_message(&mut reader).await;
            assert_eq!(thread["method"], "thread/start");
            assert_eq!(thread["params"]["approvalPolicy"], "never");
            assert_eq!(thread["params"]["sandbox"], "read-only");
            assert_eq!(thread["params"]["ephemeral"], true);
            respond(
                &mut writer,
                &thread,
                json!({ "thread": { "id": "thread-1" } }),
            )
            .await;

            let turn = read_message(&mut reader).await;
            assert_eq!(turn["method"], "turn/start");
            assert_eq!(turn["params"]["threadId"], "thread-1");
            assert_eq!(turn["params"]["input"][0]["text"], "Say hi");
            respond(&mut writer, &turn, json!({ "turn": { "id": "turn-1" } })).await;
            send(
                &mut writer,
                json!({
                    "method": "item/completed",
                    "params": {
                        "threadId": "thread-1",
                        "turnId": "turn-1",
                        "completedAtMs": 1,
                        "item": { "id": "item-1", "type": "agentMessage", "text": "Hi." }
                    }
                }),
            )
            .await;
            send(
                &mut writer,
                json!({
                    "method": "turn/completed",
                    "params": {
                        "threadId": "thread-1",
                        "turn": { "id": "turn-1", "items": [], "status": "completed", "error": null }
                    }
                }),
            )
            .await;
        });

        let (reader, writer) = tokio::io::split(client_io);
        let client = CodexClient::connect(reader, writer, Duration::from_secs(1))
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        let turn = client
            .complete_text("Say hi", None, Duration::from_secs(1))
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(turn.thread_id, "thread-1");
        assert_eq!(turn.turn_id, "turn-1");
        assert_eq!(turn.text, "Hi.");
        server.await.unwrap_or_else(|error| panic!("{error}"));
    }

    /// A server-initiated request used to end the read loop, and with it
    /// every request issued for the rest of the process's life. It is
    /// answered instead, and the connection carries on.
    #[tokio::test]
    async fn a_server_initiated_request_is_refused_without_breaking_the_connection() {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let (reader, mut writer) = tokio::io::split(server_io);
            let mut reader = BufReader::new(reader);
            let initialize = read_message(&mut reader).await;
            respond(&mut writer, &initialize, json!({})).await;
            let _initialized = read_message(&mut reader).await;

            // The app-server asks its client for something Neo does not
            // serve.
            send(
                &mut writer,
                json!({ "id": 9_001, "method": "item/requestApproval", "params": {} }),
            )
            .await;

            // Two messages come back, in either order: the refusal, because a
            // request has to be answered, and the client's own next request,
            // because the connection has to have survived answering it.
            let mut refused = false;
            let mut served = false;
            for _ in 0..2 {
                let message = read_message(&mut reader).await;
                if message["id"] == json!(9_001) {
                    assert_eq!(message["error"]["code"], JSONRPC_METHOD_NOT_FOUND);
                    refused = true;
                } else {
                    assert_eq!(message["method"], "account/read");
                    respond(
                        &mut writer,
                        &message,
                        json!({ "account": null, "requiresOpenaiAuth": false }),
                    )
                    .await;
                    served = true;
                }
            }
            assert!(refused, "the server-initiated request was never answered");
            assert!(served, "the connection did not survive the refusal");
        });

        let (reader, writer) = tokio::io::split(client_io);
        let client = CodexClient::connect(reader, writer, Duration::from_secs(5))
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        let account = client
            .read_account(false)
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(account.status, ProviderAccountStatus::SignedOut);
        server.await.unwrap_or_else(|error| panic!("{error}"));
    }

    /// Every request after the read loop died waited out the whole
    /// `request_timeout` and then reported `Timeout` — the wrong reason, for
    /// 20 s, once per call, forever. The first request here is in flight when
    /// the loop dies, so it is answered by the drain; its answer is the proof
    /// the latch is up, which makes the second request's verdict
    /// deterministic rather than a race with the reader.
    #[tokio::test]
    async fn a_request_after_the_read_loop_died_reports_the_exit_and_not_a_timeout() {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let (reader, mut writer) = tokio::io::split(server_io);
            let mut reader = BufReader::new(reader);
            let initialize = read_message(&mut reader).await;
            respond(&mut writer, &initialize, json!({})).await;
            let _initialized = read_message(&mut reader).await;

            let first = read_message(&mut reader).await;
            assert_eq!(first["method"], "account/read");
            // Answered with a line that is not JSON, which ends the read loop
            // for good. The pipe is left open, so the latch is the only thing
            // that can tell a later caller anything.
            writer
                .write_all(b"{ not json\n")
                .await
                .unwrap_or_else(|error| panic!("{error}"));
            let _held_open = reader.read_line(&mut String::new()).await;
        });

        let (reader, writer) = tokio::io::split(client_io);
        // A timeout far longer than this test can take, so passing cannot be
        // the timeout firing.
        let client = CodexClient::connect(reader, writer, Duration::from_secs(120))
            .await
            .unwrap_or_else(|error| panic!("{error}"));

        match client.read_account(false).await {
            Err(CodexError::Timeout { .. }) => {
                panic!("the drain did not answer the in-flight request")
            }
            Err(_) => {}
            Ok(_) => panic!("a connection that died mid-request cannot answer"),
        }
        match client.read_account(false).await {
            Err(CodexError::Exited(reason)) => {
                assert!(reason.contains("invalid JSON"), "{reason}");
            }
            other => panic!("expected an exit, got {other:?}"),
        }
        server.abort();
    }

    async fn read_message(reader: &mut BufReader<tokio::io::ReadHalf<DuplexStream>>) -> Value {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        serde_json::from_str(&line).unwrap_or_else(|error| panic!("{error}"))
    }

    async fn respond(
        writer: &mut tokio::io::WriteHalf<DuplexStream>,
        request: &Value,
        result: Value,
    ) {
        send(writer, json!({ "id": request["id"], "result": result })).await;
    }

    async fn send(writer: &mut tokio::io::WriteHalf<DuplexStream>, message: Value) {
        let mut encoded = serde_json::to_vec(&message).unwrap_or_else(|error| panic!("{error}"));
        encoded.push(b'\n');
        writer
            .write_all(&encoded)
            .await
            .unwrap_or_else(|error| panic!("{error}"));
    }
}
