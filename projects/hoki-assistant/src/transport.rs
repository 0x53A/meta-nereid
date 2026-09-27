use crate::wire;
use anyhow::{bail, Context, Result};
use bluer::rfcomm::{Profile, Role};
use futures::StreamExt;
use serde_json::{json, Value};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
    sync::mpsc,
};
#[derive(Clone, Debug)]
pub struct Update {
    pub status: String,
    pub detail: String,
    pub listening: bool,
    pub busy: bool,
}
pub fn update(
    tx: &mpsc::UnboundedSender<Update>,
    status: &str,
    detail: &str,
    listening: bool,
    busy: bool,
) {
    let _ = tx.send(Update {
        status: status.into(),
        detail: detail.into(),
        listening,
        busy,
    });
}
pub enum Control {
    Start,
    Send,
    Cancel,
    Shutdown,
}
struct AbortOnDrop(tokio::task::JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
fn phone_address() -> Result<bluer::Address> {
    let path = PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| "/home/ceres".into()))
        .join(".config/hoki/argyroneta.json");
    let config: Value = serde_json::from_slice(
        &std::fs::read(&path)
            .context("Configure the paired phone in ~/.config/hoki/argyroneta.json")?,
    )?;
    config["phone"]
        .as_str()
        .context("Missing phone Bluetooth address")?
        .parse()
        .context("Invalid phone Bluetooth address")
}
pub async fn worker(
    mut controls: mpsc::UnboundedReceiver<Control>,
    events: mpsc::UnboundedSender<Update>,
) {
    loop {
        match controls.recv().await {
            Some(Control::Start) => {}
            Some(Control::Shutdown) | None => return,
            _ => continue,
        }
        update(
            &events,
            "Connecting",
            "Keep the phone within Bluetooth range",
            false,
            true,
        );
        let (finish_tx, finish_rx) = mpsc::channel(1);
        let session = run(finish_rx, &events);
        tokio::pin!(session);
        loop {
            tokio::select! {
                result=&mut session => {if let Err(e)=result {update(&events,"Unavailable",&format!("{e:#}"),false,false);} break;}
                command=controls.recv()=>match command {
                    Some(Control::Send)=>{let _=finish_tx.try_send(());},
                    Some(Control::Start)=>{}, // Do not replay an in-flight request.
                    Some(Control::Shutdown)=>return,
                    Some(Control::Cancel)|None=>{update(&events,"Cancelled","Ready for another request",false,false);break;}
                }
            }
        }
        // Dropping the session closes RFCOMM and kills capture/playback children.
    }
}
async fn run(mut finish: mpsc::Receiver<()>, events: &mpsc::UnboundedSender<Update>) -> Result<()> {
    let _inhibitor =
        tokio::task::spawn_blocking(|| -> Result<Option<crate::sleep_client::Client>> {
            if !std::path::Path::new(crate::sleep_client::SOCKET).exists() {
                return Ok(None);
            }
            let mut client = crate::sleep_client::Client::connect()?;
            client.inhibit(true, true, "Voice assistant")?;
            Ok(Some(client))
        })
        .await??;
    let address = phone_address()?;
    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    let device = adapter.device(address)?;
    if !device.is_paired().await? {
        bail!("Pair the phone in Bluetooth settings first")
    }
    let uuid = uuid::Uuid::parse_str(wire::UUID)?;
    let mut profile = session
        .register_profile(Profile {
            uuid,
            role: Some(Role::Client),
            require_authentication: Some(true),
            auto_connect: Some(false),
            ..Default::default()
        })
        .await?;
    let connection = async {
        let connect = device.connect_profile(&uuid);
        tokio::pin!(connect);
        let incoming = profile.next();
        tokio::pin!(incoming);
        let (_, stream) = tokio::try_join!(
            async { connect.await.map_err(anyhow::Error::from) },
            async {
                let request = incoming.await.context("Bluetooth profile closed")?;
                if request.device() != address {
                    bail!("Unexpected Bluetooth peer")
                }
                Ok::<_, anyhow::Error>(request.accept()?)
            }
        )?;
        Ok::<_, anyhow::Error>(stream)
    };
    let stream = tokio::time::timeout(Duration::from_secs(15), connection)
        .await
        .context("Phone connection timed out")??;
    let (mut reader, mut writer) = stream.into_split();
    wire::write(
        &mut writer,
        wire::HELLO,
        json!({"version":1,"rate":16000,"channels":1,"encoding":"s16le"})
            .to_string()
            .as_bytes(),
    )
    .await?;
    let (packets_tx, mut packets) = mpsc::channel(8);
    let _reader = AbortOnDrop(tokio::spawn(async move {
        loop {
            let packet = wire::read(&mut reader).await;
            let failed = packet.is_err();
            if packets_tx.send(packet).await.is_err() || failed {
                break;
            }
        }
    }));
    let deadline = tokio::time::sleep(Duration::from_secs(120));
    tokio::pin!(deadline);
    let mut capture: Option<Child> = None;
    let mut recording = false;
    let mut ended = false;
    let mut captured = 0usize;
    let mut carry = Vec::new();
    let mut pcm = [0u8; 3200];
    let mut playback: Option<Child> = None;
    let mut reply_bytes = 0usize;
    let final_text = loop {
        tokio::select! {
            _=&mut deadline=>bail!("Assistant request timed out"),
            _=finish.recv(),if recording=> {
                stop_capture(&mut capture).await;recording=false;ended=true;
                wire::write(&mut writer,wire::END,&[]).await?;
                update(events,"Thinking","Waiting for your phone",false,true);
            }
            count=async {capture.as_mut().unwrap().stdout.as_mut().unwrap().read(&mut pcm).await},if recording=>{
                let count=count?;if count==0 {bail!("Watch microphone stopped")}
                carry.extend_from_slice(&pcm[..count]);
                let even=carry.len() & !1;
                if even == 0 {continue;}
                captured+=even;
                if captured>16000*2*60 {bail!("Recording exceeded one minute")}
                tokio::time::timeout(Duration::from_secs(3),wire::write(&mut writer,wire::PCM,&carry[..even])).await.context("Phone stopped accepting audio")??;
                carry.drain(..even);
            }
            packet=packets.recv()=>{
                let (kind,data)=packet.context("Phone disconnected")??;
                match kind {
                    wire::READY=>{
                        if capture.is_some()||ended {bail!("Unexpected recording restart")}
                        capture=Some(Command::new("parec").args(["--raw","--device=hoki_microphone","--format=s16le","--rate=16000","--channels=1","--latency-msec=100","--client-name=hoki-argyroneta"])
                            .stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true).spawn().context("Could not open watch microphone")?);
                        recording=true;
                        update(events,"Listening","Speak now. Tap Send when finished.",true,true);
                    },
                    wire::STATE=>{
                        let v:Value=serde_json::from_slice(&data)?;
                        let phase=v["phase"].as_str().unwrap_or("Thinking");
                        if phase!="Listening" && recording {stop_capture(&mut capture).await;recording=false;ended=true;}
                        update(events,phase,v["text"].as_str().unwrap_or(""),recording,true);
                    },
                    wire::AUDIO_FORMAT=>{
                        if playback.is_some() {bail!("Duplicate audio format")}
                        let v:Value=serde_json::from_slice(&data)?;
                        let rate=v["rate"].as_u64().context("Missing response sample rate")?;
                        let channels=v["channels"].as_u64().context("Missing response channels")?;
                        if !(8000..=48000).contains(&rate)||!(1..=2).contains(&channels)||v["encoding"]!="s16le" {bail!("Unsupported response audio")}
                        playback=Some(Command::new("pacat").args(["--playback","--raw","--device=hoki_speaker","--volume=65536","--format=s16le",&format!("--rate={rate}"),&format!("--channels={channels}"),"--client-name=hoki-argyroneta","--stream-name=Assistant reply"])
                            .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true).spawn()?);
                    },
                    wire::AUDIO=>{
                        reply_bytes+=data.len();if reply_bytes>8*1024*1024||data.len()%2!=0 {bail!("Invalid response audio size")}
                        let player=playback.as_mut().context("Audio arrived without format")?;
                        tokio::time::timeout(Duration::from_secs(3),player.stdin.as_mut().unwrap().write_all(&data)).await??;
                    },
                    wire::DONE=>{
                        break String::from_utf8(data)?;
                    },
                    wire::ERROR=>bail!("{}",String::from_utf8_lossy(&data)),
                    _=>bail!("Unexpected phone message {kind}"),
                }
            }
        }
    };
    stop_capture(&mut capture).await;
    if let Some(mut player) = playback {
        drop(player.stdin.take());
        tokio::time::timeout(Duration::from_secs(15), player.wait()).await??;
    }
    update(events, "Reply", &final_text, false, false);
    Ok(())
}
async fn stop_capture(capture: &mut Option<Child>) {
    if let Some(mut child) = capture.take() {
        let _ = child.kill().await;
    }
}
