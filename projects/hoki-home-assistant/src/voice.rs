//! One authenticated Home Assistant Assist pipeline run per tap.
use crate::{sleep_client, App, Config};
use anyhow::{bail, Context, Result};
use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use serde_json::{json, Value};
use slint::Weak;
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    sync::mpsc,
};
use tokio_tungstenite::{
    connect_async_tls_with_config,
    tungstenite::{protocol::WebSocketConfig, Message},
    MaybeTlsStream, WebSocketStream,
};

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

const RECORD_LIMIT: usize = 16_000 * 2 * 60;
const SESSION_TIMEOUT: Duration = Duration::from_secs(120);

pub enum Control {
    Start,
    Send,
    Cancel,
    Shutdown,
}

fn update(weak: &Weak<App>, status: &str, detail: &str, busy: bool, listening: bool) {
    let status = status.to_owned();
    let detail = detail.to_owned();
    weak.upgrade_in_event_loop(move |app| {
        app.set_voice_status(status.into());
        app.set_voice_detail(detail.into());
        app.set_voice_busy(busy);
        app.set_voice_listening(listening);
    })
    .ok();
}

pub async fn worker(
    mut controls: mpsc::UnboundedReceiver<Control>,
    config: Config,
    weak: Weak<App>,
) {
    while let Some(command) = controls.recv().await {
        match command {
            Control::Start => {}
            Control::Shutdown => return,
            _ => continue,
        }
        update(&weak, "Connecting", "Opening Home Assistant", true, false);
        let (finish_tx, finish_rx) = mpsc::channel(1);
        let session = run(&config, finish_rx, &weak);
        tokio::pin!(session);
        loop {
            tokio::select! {
                result = &mut session => {
                    if let Err(error) = result {
                        update(&weak, "Unavailable", &format!("{error:#}"), false, false);
                    }
                    break;
                }
                command = controls.recv() => match command {
                    Some(Control::Send) => { let _ = finish_tx.try_send(()); }
                    Some(Control::Start) => {},
                    Some(Control::Cancel) => {
                        // Dropping the run closes the socket and kills parec.
                        break;
                    }
                    Some(Control::Shutdown) | None => return,
                }
            }
        }
    }
}

fn websocket_url(config: &Config) -> Result<String> {
    let mut url = reqwest::Url::parse(&config.url)?;
    url.set_scheme(if url.scheme() == "https" { "wss" } else { "ws" })
        .map_err(|_| anyhow::anyhow!("Invalid HA URL scheme"))?;
    let base = url.path().trim_end_matches('/').to_owned();
    url.set_path(&format!("{base}/api/websocket"));
    Ok(url.to_string())
}

fn text_message(value: Value) -> Message {
    Message::Text(value.to_string().into())
}

async fn receive_json<S>(stream: &mut S) -> Result<Value>
where
    S: futures_util::Stream<
            Item = std::result::Result<Message, tokio_tungstenite::tungstenite::Error>,
        > + Unpin,
{
    loop {
        let message = stream
            .next()
            .await
            .context("HA closed the voice connection")??;
        match message {
            Message::Text(text) => {
                return serde_json::from_str(&text).context("Invalid HA voice message")
            }
            Message::Close(_) => bail!("HA closed the voice connection"),
            Message::Binary(_) => bail!("Unexpected HA voice binary message"),
            _ => {}
        }
    }
}

fn event_kind(message: &Value) -> Option<&str> {
    if message["type"] == "event" && message["id"] == 1 {
        message["event"]["type"].as_str()
    } else {
        None
    }
}

fn pipeline_error(message: &Value) -> String {
    message["event"]["data"]["message"]
        .as_str()
        .or_else(|| message["event"]["data"]["code"].as_str())
        .unwrap_or("Home Assistant voice error")
        .chars()
        .take(160)
        .collect()
}

fn handler_id(message: &Value) -> Result<u8> {
    let id = message["event"]["data"]["runner_data"]["stt_binary_handler_id"]
        .as_u64()
        .context("HA omitted the audio handler ID")?;
    u8::try_from(id).context("Invalid HA audio handler ID")
}

fn pcm_frame(handler: u8, pcm: &[u8]) -> Result<Message> {
    if pcm.is_empty() || pcm.len() > 3200 || pcm.len() % 2 != 0 {
        bail!("Invalid microphone frame");
    }
    let mut frame = Vec::with_capacity(pcm.len() + 1);
    frame.push(handler);
    frame.extend_from_slice(pcm);
    Ok(Message::Binary(frame.into()))
}

async fn stop_capture(capture: &mut Option<Child>) {
    if let Some(mut child) = capture.take() {
        let _ = child.kill().await;
    }
}

async fn connect_pipeline(
    config: &Config,
) -> Result<(SplitSink<Socket, Message>, SplitStream<Socket>, u8)> {
    let mut ws_config = WebSocketConfig::default();
    ws_config.max_message_size = Some(256 * 1024);
    ws_config.max_frame_size = Some(128 * 1024);
    let url = websocket_url(config)?;
    let (mut socket, _) = tokio::time::timeout(
        Duration::from_secs(8),
        connect_async_tls_with_config(url.as_str(), Some(ws_config), false, None),
    )
    .await
    .context("HA connection timed out")?
    .context("Cannot connect to HA voice API")?;
    let hello = tokio::time::timeout(Duration::from_secs(8), receive_json(&mut socket)).await??;
    if hello["type"] != "auth_required" {
        bail!("Unexpected HA authentication greeting");
    }
    socket
        .send(text_message(
            json!({"type":"auth", "access_token":config.token}),
        ))
        .await?;
    let authenticated =
        tokio::time::timeout(Duration::from_secs(8), receive_json(&mut socket)).await??;
    if authenticated["type"] != "auth_ok" {
        bail!("HA rejected voice authentication");
    }

    let mut request = json!({
        "id": 1,
        "type": "assist_pipeline/run",
        "start_stage": "stt",
        "end_stage": "intent",
        "input": {"sample_rate": 16000},
        "timeout": 120
    });
    if let Some(pipeline) = config
        .voice
        .as_ref()
        .and_then(|voice| voice.pipeline.as_ref())
    {
        request["pipeline"] = json!(pipeline);
    }
    let (mut sink, mut stream) = socket.split();
    sink.send(text_message(request)).await?;
    let mut accepted = false;
    let mut audio_handler = None;
    let mut stt_started = false;
    while !accepted || audio_handler.is_none() || !stt_started {
        let message = tokio::time::timeout(Duration::from_secs(10), receive_json(&mut stream))
            .await
            .context("HA did not start speech recognition")??;
        if message["type"] == "result" && message["id"] == 1 {
            if message["success"] != true {
                let reason = message["error"]["message"]
                    .as_str()
                    .unwrap_or("Pipeline unavailable");
                bail!("HA: {}", reason.chars().take(160).collect::<String>());
            }
            accepted = true;
        }
        match event_kind(&message) {
            Some("run-start") => audio_handler = Some(handler_id(&message)?),
            Some("stt-start") => stt_started = true,
            Some("error") => bail!("HA: {}", pipeline_error(&message)),
            _ => {}
        }
    }
    Ok((
        sink,
        stream,
        audio_handler.context("Missing audio handler")?,
    ))
}

async fn run(config: &Config, mut finish: mpsc::Receiver<()>, weak: &Weak<App>) -> Result<()> {
    let _inhibitor = tokio::task::spawn_blocking(|| -> Result<Option<sleep_client::Client>> {
        if !Path::new(sleep_client::SOCKET).exists() {
            return Ok(None);
        }
        let mut client = sleep_client::Client::connect()?;
        client.inhibit(true, true, "Home Assistant voice")?;
        Ok(Some(client))
    })
    .await??;
    let (mut sink, mut stream, handler) = connect_pipeline(config).await?;
    let mut capture = Some(
        Command::new("parec")
            .args([
                "--raw",
                "--device=hoki_microphone",
                "--format=s16le",
                "--rate=16000",
                "--channels=1",
                "--latency-msec=100",
                "--client-name=hoki-home-assistant",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("Could not open watch microphone")?,
    );
    update(
        weak,
        "Listening",
        "Speak now. Tap Send when finished.",
        true,
        true,
    );
    let deadline = tokio::time::sleep(SESSION_TIMEOUT);
    tokio::pin!(deadline);
    let mut recording = true;
    let mut captured = 0usize;
    let mut pending = Vec::new();
    let mut pcm = [0u8; 3200];
    let mut transcript = String::new();
    let mut reply = String::new();
    loop {
        tokio::select! {
            _ = &mut deadline => bail!("Voice request timed out"),
            _ = finish.recv(), if recording => {
                stop_capture(&mut capture).await;
                recording = false;
                sink.send(Message::Binary(vec![handler].into())).await?;
                update(weak, "Thinking", "Waiting for Home Assistant", true, false);
            }
            count = async { capture.as_mut().unwrap().stdout.as_mut().unwrap().read(&mut pcm).await }, if recording => {
                let count = count?;
                if count == 0 { bail!("Watch microphone stopped"); }
                pending.extend_from_slice(&pcm[..count]);
                let even = pending.len() & !1;
                if even == 0 { continue; }
                captured += even;
                if captured > RECORD_LIMIT { bail!("Recording reached one minute"); }
                tokio::time::timeout(Duration::from_secs(3), sink.send(pcm_frame(handler, &pending[..even])?))
                    .await.context("HA stopped accepting audio")??;
                pending.drain(..even);
            }
            message = receive_json(&mut stream) => {
                let message = message?;
                match event_kind(&message) {
                    Some("stt-vad-start") => update(weak, "Listening", "Speech detected", true, true),
                    Some("stt-vad-end") | Some("stt-end") if recording => {
                        stop_capture(&mut capture).await;
                        recording = false;
                        // HA may have finished VAD itself; its handler still accepts EOF.
                        sink.send(Message::Binary(vec![handler].into())).await?;
                        if event_kind(&message) == Some("stt-end") {
                            transcript = message["event"]["data"]["stt_output"]["text"]
                                .as_str().unwrap_or("").to_owned();
                            update(weak, "Heard", &transcript, true, false);
                        } else {
                            update(weak, "Thinking", "Waiting for Home Assistant", true, false);
                        }
                    }
                    Some("stt-end") => {
                        transcript = message["event"]["data"]["stt_output"]["text"]
                            .as_str().unwrap_or("").to_owned();
                        update(weak, "Heard", &transcript, true, false);
                    }
                    Some("intent-end") => {
                        reply = message["event"]["data"]["intent_output"]["response"]["speech"]["plain"]["speech"]
                            .as_str().unwrap_or("").to_owned();
                        update(weak, "Reply", if reply.is_empty() { "No text reply" } else { &reply }, true, false);
                    }
                    Some("error") => bail!("HA: {}", pipeline_error(&message)),
                    Some("run-end") => break,
                    _ => {},
                }
            }
        }
    }
    stop_capture(&mut capture).await;
    let detail = if !reply.is_empty() {
        reply
    } else if !transcript.is_empty() {
        transcript
    } else {
        "No reply from Home Assistant".into()
    };
    update(weak, "Reply", &detail, false, false);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_websocket_path_and_rejects_bad_handler() {
        let config = Config {
            url: "https://ha.test/prefix/".into(),
            token: "secret".into(),
            actions: vec![],
            voice: None,
        };
        assert_eq!(
            websocket_url(&config).unwrap(),
            "wss://ha.test/prefix/api/websocket"
        );
        assert!(handler_id(
            &json!({"event":{"data":{"runner_data":{"stt_binary_handler_id":256}}}})
        )
        .is_err());
        assert_eq!(
            pcm_frame(7, &[0x34, 0x12]).unwrap().into_data(),
            vec![7, 0x34, 0x12]
        );
        assert!(pcm_frame(7, &[0x34]).is_err());
    }

    #[tokio::test]
    async fn authenticates_and_waits_for_stt_start() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            socket
                .send(text_message(json!({"type":"auth_required"})))
                .await
                .unwrap();
            let auth = receive_json(&mut socket).await.unwrap();
            assert_eq!(auth, json!({"type":"auth","access_token":"secret"}));
            socket
                .send(text_message(json!({"type":"auth_ok"})))
                .await
                .unwrap();
            let run = receive_json(&mut socket).await.unwrap();
            assert_eq!(run["type"], "assist_pipeline/run");
            assert_eq!(run["start_stage"], "stt");
            assert_eq!(run["end_stage"], "intent");
            assert_eq!(run["input"]["sample_rate"], 16000);
            assert_eq!(run["pipeline"], "test-pipeline");
            socket
                .send(text_message(json!({"id":1,"type":"result","success":true})))
                .await
                .unwrap();
            socket.send(text_message(json!({"id":1,"type":"event","event":{"type":"run-start","data":{"runner_data":{"stt_binary_handler_id":7}}}}))).await.unwrap();
            socket
                .send(text_message(
                    json!({"id":1,"type":"event","event":{"type":"stt-start","data":{}}}),
                ))
                .await
                .unwrap();
        });
        let config = Config {
            url: format!("http://{address}"),
            token: "secret".into(),
            actions: vec![],
            voice: Some(crate::VoiceConfig {
                pipeline: Some("test-pipeline".into()),
            }),
        };
        let (_sink, _stream, handler) = connect_pipeline(&config).await.unwrap();
        assert_eq!(handler, 7);
        server.await.unwrap();
    }
}
