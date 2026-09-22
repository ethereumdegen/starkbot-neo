use std::{
    collections::HashMap,
    convert::Infallible,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Sse, sse::Event},
    routing::{get, post},
};
use futures_util::{Stream, StreamExt, stream};
use neo_agent::runtime::Runtime;
use neo_core::{AppEvent, Envelope, RunId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use spice_framework::report::{SuiteReport, TestReport};
use tokio::sync::{Mutex, RwLock, broadcast};
use tokio_stream::wrappers::BroadcastStream;
use tokio_util::sync::CancellationToken;

use crate::{CaseListing, Selection, list_cases, run_suite};

const MAX_LIVE_EVENTS: usize = 4_000;

#[derive(Clone)]
struct WebState {
    runtime: Arc<Runtime>,
    runs: Arc<RwLock<HashMap<String, RunView>>>,
    active: Arc<Mutex<Option<ActiveRun>>>,
    next_id: Arc<AtomicU64>,
    updates: broadcast::Sender<(String, String)>,
}

struct ActiveRun {
    id: String,
    cancel: CancellationToken,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RunStatus {
    Running,
    Passed,
    Failed,
    JudgeUnavailable,
    Cancelled,
    Error,
}

fn completed_status(report: &SuiteReport) -> RunStatus {
    completed_status_from_tests(&report.tests, report.failed)
}

fn completed_status_from_tests(tests: &[TestReport], failed: usize) -> RunStatus {
    if failed == 0 {
        return RunStatus::Passed;
    }
    let provider_error = tests.iter().any(|test| {
        test.judge_results
            .iter()
            .any(|judge| judge.reason.starts_with("judge error:"))
    });
    let otherwise_passed = tests.iter().all(|test| {
        test.error.is_none()
            && test
                .assertion_results
                .iter()
                .all(|assertion| assertion.passed)
            && test
                .judge_results
                .iter()
                .all(|judge| judge.passed || judge.reason.starts_with("judge error:"))
    });
    if provider_error && otherwise_passed {
        RunStatus::JudgeUnavailable
    } else {
        RunStatus::Failed
    }
}

#[derive(Clone, Debug, Serialize)]
struct RunView {
    id: String,
    case_id: String,
    once: bool,
    status: RunStatus,
    events: Vec<Envelope>,
    traces: Vec<Value>,
    report: Option<SuiteReport>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StartRequest {
    case_id: String,
    #[serde(default = "default_once")]
    once: bool,
}

const fn default_once() -> bool {
    true
}

#[derive(Debug, Serialize)]
struct StartResponse {
    id: String,
}

/// Serve the interactive Spice test catalog and runner.
pub async fn serve(runtime: Arc<Runtime>, address: SocketAddr) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(address).await?;
    serve_listener(runtime, listener).await
}

/// Serve on an already-bound listener, allowing a CLI to report the actual
/// address before it starts accepting requests.
pub async fn serve_listener(
    runtime: Arc<Runtime>,
    listener: tokio::net::TcpListener,
) -> std::io::Result<()> {
    let (updates, _) = broadcast::channel(256);
    let state = WebState {
        runtime,
        runs: Arc::new(RwLock::new(HashMap::new())),
        active: Arc::new(Mutex::new(None)),
        next_id: Arc::new(AtomicU64::new(1)),
        updates,
    };
    let app = Router::new()
        .route("/", get(index))
        .route("/api/cases", get(cases))
        .route("/api/runs", post(start_run))
        .route("/api/runs/{id}", get(get_run))
        .route("/api/runs/{id}/events", get(run_events))
        .route("/api/runs/{id}/cancel", post(cancel_run))
        .with_state(state);
    axum::serve(listener, app).await
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn cases() -> Json<Vec<CaseListing>> {
    Json(list_cases())
}

async fn start_run(
    State(state): State<WebState>,
    Json(request): Json<StartRequest>,
) -> Result<(StatusCode, Json<StartResponse>), ApiError> {
    let case = list_cases()
        .into_iter()
        .find(|case| case.id == request.case_id)
        .ok_or_else(|| ApiError::not_found("unknown Spice test"))?;
    if !case.runnable() {
        return Err(ApiError::conflict(format!(
            "{} is not installed on this machine",
            case.app.label()
        )));
    }

    let mut active = state.active.lock().await;
    if let Some(run) = active.as_ref() {
        return Err(ApiError::conflict(format!(
            "run {} is still active; cancel it before starting another",
            run.id
        )));
    }

    let id = format!("run-{}", state.next_id.fetch_add(1, Ordering::Relaxed));
    let cancel = CancellationToken::new();
    *active = Some(ActiveRun {
        id: id.clone(),
        cancel: cancel.clone(),
    });
    state.runs.write().await.insert(
        id.clone(),
        RunView {
            id: id.clone(),
            case_id: case.id.clone(),
            once: request.once,
            status: RunStatus::Running,
            events: Vec::new(),
            traces: Vec::new(),
            report: None,
            error: None,
        },
    );
    publish_view(&state, &id).await;

    let task_state = state.clone();
    let task_id = id.clone();
    tokio::spawn(async move {
        execute_run(task_state, task_id, case.id, request.once, cancel).await;
    });

    Ok((StatusCode::ACCEPTED, Json(StartResponse { id })))
}

async fn execute_run(
    state: WebState,
    id: String,
    case_id: String,
    once: bool,
    cancel: CancellationToken,
) {
    let mut receiver = state.runtime.subscribe();
    let collector_state = state.clone();
    let collector_id = id.clone();
    let collector_done = CancellationToken::new();
    let collector_stop = collector_done.clone();
    let collector = tokio::spawn(async move {
        loop {
            tokio::select! {
                () = collector_stop.cancelled() => break,
                received = receiver.recv() => match received {
                    Ok(envelope) if is_trace_event(&envelope.event) => {
                        let mut runs = collector_state.runs.write().await;
                        if let Some(run) = runs.get_mut(&collector_id) {
                            if run.events.len() == MAX_LIVE_EVENTS {
                                run.events.remove(0);
                            }
                            run.events.push(envelope);
                        }
                        drop(runs);
                        publish_view(&collector_state, &collector_id).await;
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    });

    let selection = Selection {
        filter: None,
        exact_id: Some(case_id.clone()),
        tags: Vec::new(),
        once,
    };
    let result = run_suite(&state.runtime, selection, RunId::new(), &cancel).await;
    collector_done.cancel();
    let _ = collector.await;

    let traces = crate::saved_traces(&state.runtime, &case_id);
    let mut runs = state.runs.write().await;
    if let Some(run) = runs.get_mut(&id) {
        run.traces = traces;
        match result {
            Ok(report) => {
                run.status = completed_status(&report);
                run.report = Some(report);
            }
            Err(error) if cancel.is_cancelled() => {
                run.status = RunStatus::Cancelled;
                run.error = Some(error.to_string());
            }
            Err(error) => {
                run.status = RunStatus::Error;
                run.error = Some(error.to_string());
            }
        }
    }
    drop(runs);
    let mut active = state.active.lock().await;
    if active.as_ref().is_some_and(|run| run.id == id) {
        *active = None;
    }
    drop(active);
    publish_view(&state, &id).await;
}

fn is_trace_event(event: &AppEvent) -> bool {
    matches!(
        event,
        AppEvent::TurnStarted { .. }
            | AppEvent::TurnStep { .. }
            | AppEvent::TurnStepDone { .. }
            | AppEvent::TurnNote { .. }
            // The final answer arrives in `TurnFinished`. Forwarding every
            // token delta would repeatedly serialize the complete run and can
            // outrun a browser's SSE buffer on long tool observations.
            | AppEvent::TurnCost { .. }
            | AppEvent::TurnFinished { .. }
            | AppEvent::TurnFailed { .. }
            | AppEvent::EvalCase { .. }
    )
}

async fn get_run(
    State(state): State<WebState>,
    Path(id): Path<String>,
) -> Result<Json<RunView>, ApiError> {
    let runs = state.runs.read().await;
    runs.get(&id)
        .cloned()
        .map(Json)
        .ok_or_else(|| ApiError::not_found("unknown evaluation run"))
}

async fn run_events(
    State(state): State<WebState>,
    Path(id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let initial = {
        let runs = state.runs.read().await;
        let view = runs
            .get(&id)
            .ok_or_else(|| ApiError::not_found("unknown evaluation run"))?;
        serde_json::to_string(view).map_err(ApiError::internal)?
    };
    let wanted_id = id.clone();
    let live = BroadcastStream::new(state.updates.subscribe()).filter_map(move |message| {
        let wanted_id = wanted_id.clone();
        async move {
            match message {
                Ok((run_id, body)) if run_id == wanted_id => Some(Ok(Event::default().data(body))),
                _ => None,
            }
        }
    });
    let initial = stream::once(async move { Ok(Event::default().data(initial)) });
    Ok(Sse::new(initial.chain(live))
        .keep_alive(axum::response::sse::KeepAlive::default().text("starkbot-spice")))
}

async fn cancel_run(
    State(state): State<WebState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let active = state.active.lock().await;
    let run = active
        .as_ref()
        .filter(|run| run.id == id)
        .ok_or_else(|| ApiError::conflict("that run is not active"))?;
    run.cancel.cancel();
    Ok(StatusCode::ACCEPTED)
}

async fn publish_view(state: &WebState, id: &str) {
    let body = {
        let runs = state.runs.read().await;
        runs.get(id)
            .and_then(|view| serde_json::to_string(view).ok())
    };
    if let Some(body) = body {
        let _ = state.updates.send((id.to_owned(), body));
    }
}

struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
        }
    }

    fn internal(error: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: error.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

const INDEX_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Starkbot Spice Lab</title>
<style>
:root{color-scheme:dark;--bg:#090b10;--panel:#11151d;--panel2:#161b25;--line:#263041;--text:#edf1f7;--muted:#8e9bad;--amber:#f5b942;--green:#50d890;--red:#ff6b72;--blue:#69a8ff}*{box-sizing:border-box}body{margin:0;background:radial-gradient(circle at 15% 0,#172235 0,transparent 34%),var(--bg);color:var(--text);font:14px/1.5 ui-monospace,SFMono-Regular,Menlo,monospace}header{height:72px;display:flex;align-items:center;gap:14px;padding:0 24px;border-bottom:1px solid var(--line);background:#090b10d9;backdrop-filter:blur(12px);position:sticky;top:0;z-index:3}.mark{width:34px;height:34px;border:1px solid var(--amber);display:grid;place-items:center;color:var(--amber);font-weight:800;box-shadow:0 0 24px #f5b94230}.title{font:700 17px/1 system-ui}.subtitle{color:var(--muted);margin-top:5px;font-size:11px}.live{margin-left:auto;color:var(--green);font-size:11px;letter-spacing:.12em}.shell{min-height:calc(100vh - 72px)}main{padding:28px;max-width:1200px;width:100%;margin:auto}.catalog-head{display:flex;align-items:end;gap:20px;justify-content:space-between;margin-bottom:18px}.catalog-title{font:700 clamp(22px,3vw,34px)/1.15 system-ui;margin:8px 0 0}.search{width:min(360px,100%);padding:11px 12px;border:1px solid var(--line);background:var(--panel);color:var(--text);outline:none}.search:focus{border-color:var(--blue)}.catalog{display:grid;grid-template-columns:repeat(auto-fit,minmax(260px,1fr));gap:8px}.case{display:block;width:100%;min-height:118px;text-align:left;border:1px solid var(--line);background:var(--panel);color:var(--text);padding:16px;cursor:pointer}.case:hover,.case:focus-visible{background:var(--panel2);border-color:var(--amber)}.case-id{font-weight:700;font-size:12px;overflow-wrap:anywhere}.meta,.tags{color:var(--muted);font-size:10px;margin-top:7px}.tag{display:inline-block;padding:2px 6px;background:#202838;color:#aeb9c9;margin:3px 3px 0 0}.unavailable{color:var(--red)}.back{border:1px solid var(--line);background:transparent;color:var(--text);padding:8px 12px;margin-bottom:20px;font:700 11px ui-monospace,monospace;cursor:pointer}.back:hover{border-color:var(--amber)}.eyebrow{color:var(--amber);letter-spacing:.14em;font-size:10px;text-transform:uppercase}.case-title{font:700 clamp(22px,3vw,34px)/1.15 system-ui;margin:8px 0 18px}.block{background:var(--panel);border:1px solid var(--line);padding:16px;margin:12px 0}.label{color:var(--muted);font-size:10px;letter-spacing:.1em;text-transform:uppercase;margin-bottom:7px}.actions{display:flex;gap:10px;align-items:center;margin:18px 0}button.action{border:0;background:var(--amber);color:#15100a;padding:11px 18px;font:700 12px ui-monospace,monospace;cursor:pointer}button.action.secondary{background:transparent;color:var(--text);border:1px solid var(--line)}button:disabled{opacity:.38;cursor:not-allowed}.status{padding:5px 9px;border:1px solid var(--line);font-size:11px}.status.running{color:var(--blue)}.status.passed{color:var(--green)}.status.failed,.status.error{color:var(--red)}.grid{display:grid;grid-template-columns:1.3fr .7fr;gap:14px}.trace{border-left:1px solid var(--line);padding:3px 0 14px 18px;position:relative}.trace:before{content:"";position:absolute;width:7px;height:7px;border-radius:50%;background:var(--blue);left:-4px;top:9px}.trace-head{display:flex;gap:10px;color:var(--blue);font-size:11px}.trace pre,.json{white-space:pre-wrap;overflow-wrap:anywhere;background:#0a0d13;padding:12px;border:1px solid #1c2431;color:#c8d1de;font:12px/1.55 ui-monospace,monospace}.judge{border-left:3px solid var(--amber)}.judge.pass{border-left-color:var(--green)}.judge.fail{border-left-color:var(--red)}.score{font:700 26px system-ui}.empty{color:var(--muted);padding:36px;text-align:center;border:1px dashed var(--line)}.status.judge_unavailable{color:var(--amber)}@media(max-width:850px){main{padding:18px}.catalog-head{align-items:stretch;flex-direction:column}.search{width:100%}.catalog{grid-template-columns:1fr}.grid{grid-template-columns:1fr}}
.page[hidden]{display:none}
</style>
</head>
<body>
<header><div class="mark">S</div><div><div class="title">Starkbot Spice Lab</div><div class="subtitle">Metalcraft trajectories · deterministic assertions · Jev judgment</div></div><div class="live">● LOCAL</div></header>
<div class="shell"><main id="index-page" class="page"><div class="catalog-head"><div><div class="eyebrow">Evaluation suite</div><h1 class="catalog-title">Tests</h1></div><input id="search" class="search" placeholder="Filter tests or tags…"></div><div id="catalog" class="catalog"><div class="empty">Loading the Spice catalog…</div></div></main><main id="show-page" class="page" hidden><button id="back" class="back">← All tests</button><div id="main"></div></main></div>
<script>
const $=s=>document.querySelector(s);let cases=[],selected=null,current=null,source=null;
const esc=v=>String(v??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
fetch('/api/cases').then(r=>r.json()).then(v=>{cases=v;drawCatalog()}).catch(e=>$('#catalog').innerHTML=`<div class="empty">${esc(e)}</div>`);
$('#search').addEventListener('input',drawCatalog);$('#back').addEventListener('click',showIndex);
function showIndex(){$('#show-page').hidden=true;$('#index-page').hidden=false;$('#search').focus()}
function showCase(){$('#index-page').hidden=true;$('#show-page').hidden=false;window.scrollTo(0,0)}
function openCase(id){const next=cases.find(c=>c.id===id);if(!next)return;if(selected?.id!==next.id){selected=next;current=null;if(source)source.close();source=null;drawCase()}showCase()}
function drawCatalog(){const q=$('#search').value.toLowerCase();const matches=cases.filter(c=>(c.id+' '+c.tags.join(' ')+' '+(c.name||'')).toLowerCase().includes(q));$('#catalog').innerHTML=matches.map(c=>`<button class="case" data-id="${esc(c.id)}"><div class="case-id">${esc(c.name||c.id)}</div><div class="meta ${c.installed?'':'unavailable'}">${esc(c.app)} · ${c.installed?'ready':'not installed'}</div><div class="tags">${c.tags.map(t=>`<span class="tag">${esc(t)}</span>`).join('')}</div></button>`).join('')||'<div class="empty">No tests match that filter.</div>';document.querySelectorAll('.case').forEach(b=>b.onclick=()=>openCase(b.dataset.id))}
function drawCase(){if(!selected)return;const judges=selected.judges.map(j=>`<div class="block judge"><div class="label">Jev rubric · threshold ${(j.threshold*100).toFixed(0)}%</div>${esc(j.rubric)}</div>`).join('')||'<div class="block"><div class="label">Judge</div>Deterministic assertions only</div>';$('#main').innerHTML=`<div class="eyebrow">${esc(selected.app)} / ${esc(selected.id)}</div><h1 class="case-title">${esc(selected.name||selected.id)}</h1><div class="block"><div class="label">User turn</div>${esc(selected.message)}</div>${judges}<div class="actions"><button id="once" class="action" ${selected.installed?'':'disabled'}>Run once</button><button id="consensus" class="action secondary" ${selected.installed?'':'disabled'}>Run consensus</button><span id="status"></span></div><div id="results"><div class="empty">Choose a run mode to inspect the complete Metalcraft conversation.</div></div>`;$('#once').onclick=()=>start(true);$('#consensus').onclick=()=>start(false)}
async function start(once){for(const b of document.querySelectorAll('.action'))b.disabled=true;$('#status').innerHTML='<span class="status running">starting</span>';const r=await fetch('/api/runs',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({case_id:selected.id,once})});const body=await r.json();if(!r.ok){$('#status').innerHTML=`<span class="status error">${esc(body.error)}</span>`;return}current=body.id;if(source)source.close();source=new EventSource(`/api/runs/${current}/events`);source.onmessage=e=>{const run=JSON.parse(e.data);renderRun(run);if(run.status!=='running'){source.close();for(const b of document.querySelectorAll('.action'))b.disabled=!selected.installed}};source.onerror=()=>{$('#status').innerHTML='<span class="status error">event stream disconnected</span>'}}
function renderRun(run){const statusLabel=run.status.replaceAll('_',' ');$('#status').innerHTML=`<span class="status ${run.status}">${esc(statusLabel)}</span>`;const test=run.report?.tests?.[0];const traces=run.traces.flatMap(t=>{const turns=(t.output?.turns||[]).map(x=>({type:`turn ${x.index+1}`,arguments:{output_text:x.output_text,tool_calls:x.tool_calls},observation:x.tool_results}));if(t.output?.final_text)turns.push({type:'final answer',arguments:t.output.final_text});return turns});const live=run.events.map(x=>x.event).filter(e=>['turn_step','turn_step_done','turn_note','turn_finished','turn_failed'].includes(e.type));const rows=traces.length?traces:live;const traceHtml=rows.map((t,i)=>{const type=t.tool_name||t.type||'step';const args=t.arguments||t.thought||t.line||t.text||'';const obs=t.observation||t.error||'';const fmt=v=>typeof v==='string'?v:JSON.stringify(v,null,2);return `<div class="trace"><div class="trace-head"><b>${i+1}</b><span>${esc(type)}</span></div>${args?`<pre>${esc(fmt(args))}</pre>`:''}${obs?`<pre>${esc(fmt(obs))}</pre>`:''}</div>`}).join('')||'<div class="empty">Waiting for the first model step…</div>';const judgeHtml=test?.judge_results?.map(j=>{const unavailable=j.reason.startsWith('judge error:');const state=unavailable?'unavailable':j.passed?'pass':'fail';const label=unavailable?'Jev unavailable':`Jev ${j.passed?'pass':'fail'}`;const score=unavailable?'No verdict':`${(j.score*100).toFixed(0)}%`;const help=unavailable?'<p><b>The deterministic checks passed, but Jev did not authenticate. Replace the TypeSafe key in Connections or run <code>neo keys set typesafe</code>, then rerun.</b></p>':'';return `<div class="block judge ${state}"><div class="label">${label}</div><div class="score">${score}</div><div>threshold ${(j.threshold*100).toFixed(0)}%</div>${help}<p>${esc(j.reason)}</p></div>`}).join('')||`<div class="block judge"><div class="label">Jev judgment</div>${run.status==='running'?'Pending until the trajectory completes.':esc(run.error||'No judge configured for this test.')}</div>`;const assertions=test?.assertion_results?.map(a=>`<div class="block"><b style="color:${a.passed?'var(--green)':'var(--red)'}">${a.passed?'PASS':'FAIL'}</b><pre class="json">${esc(JSON.stringify(a,null,2))}</pre></div>`).join('')||'';$('#results').innerHTML=`<div class="grid"><section><div class="label">Per-turn trajectory · ${rows.length} events</div>${traceHtml}</section><section><div class="label">Final evaluation</div>${judgeHtml}${assertions}${run.error?`<div class="block"><div class="label">Run error</div>${esc(run.error)}</div>`:''}</section></div>`}
</script>
</body>
</html>"##;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use spice_framework::report::{JudgeResult, TestReport};

    use super::{RunStatus, completed_status_from_tests};

    fn report_with_judge(reason: &str, passed: bool) -> TestReport {
        TestReport {
            test_id: "case".to_owned(),
            test_name: None,
            tags: Vec::new(),
            passed,
            attempts: 1,
            assertion_results: Vec::new(),
            judge_results: vec![JudgeResult {
                rubric: "Be correct".to_owned(),
                score: if passed { 1.0 } else { 0.0 },
                threshold: 0.8,
                passed,
                reason: reason.to_owned(),
            }],
            score: if passed { 1.0 } else { 0.0 },
            consensus: None,
            usage: None,
            run_duration: Some(Duration::ZERO),
            duration: Duration::ZERO,
            error: None,
        }
    }

    #[test]
    fn provider_error_is_unavailable_not_failed() {
        let report = report_with_judge("judge error: provider returned HTTP 401", false);
        assert_eq!(
            completed_status_from_tests(&[report], 1),
            RunStatus::JudgeUnavailable
        );
    }

    #[test]
    fn semantic_judge_rejection_remains_failed() {
        let report = report_with_judge("The answer invented facts.", false);
        assert_eq!(completed_status_from_tests(&[report], 1), RunStatus::Failed);
    }
}
