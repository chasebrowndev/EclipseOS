// SPDX-License-Identifier: AGPL-3.0-only

//! PipeWire producer: node "Eclipse Screen Share", `Video/Source`, raw
//! BGRx/BGRA (or RGBx/RGBA) frames pushed from the capture pump.
//!
//! The stream is connected `DRIVER | TRIGGER | ALLOC_BUFFERS | MAP_BUFFERS`.
//! `TRIGGER` is the point (ADR 0029): xdpw 0.8.3 was `DRIVER` only and only
//! re-armed capture from its `process` callback, so one frame went out and
//! the graph never cycled again. Here every captured frame calls
//! `trigger_process()` itself; nothing depends on the consumer pulling.
//!
//! Threading: the PipeWire main loop runs on its own thread. Frames cross by
//! a `pipewire::channel`; everything pw-typed stays on that thread.
//!
//! API NOTE: written against pipewire-rs 0.9 / libspa from memory (the crate
//! is not in the offline registry on the authoring host). The pure parts
//! (format choice, buffer copy) are tested in `source`/this file; the pw glue
//! needs a first real build and a `pw-dump`/`pw-cat` smoke test.

use std::cell::RefCell;
use std::io::Cursor;
use std::rc::Rc;
use std::sync::mpsc;
use std::thread::JoinHandle;

use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use spa::param::format::{FormatProperties, MediaSubtype, MediaType};
use spa::param::video::{VideoFormat, VideoInfoRaw};
use spa::param::ParamType;
use spa::pod::serialize::PodSerializer;
use spa::pod::{self, ChoiceValue, Pod, Property, PropertyFlags, Value};
use spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Fraction, Id, Rectangle, SpaTypes};

use crate::pipeline::FrameSink;
use crate::source::{Frame, PixelFormat};

pub const NODE_NAME: &str = "Eclipse Screen Share";

/// Formats to advertise for a source: native channel order, with and without
/// alpha. We never swizzle in the producer.
pub fn advertised(fmt: PixelFormat) -> [PixelFormatChoice; 2] {
    if fmt.is_bgr_order() {
        [PixelFormatChoice::Bgrx, PixelFormatChoice::Bgra]
    } else {
        [PixelFormatChoice::Rgbx, PixelFormatChoice::Rgba]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormatChoice {
    Bgrx,
    Bgra,
    Rgbx,
    Rgba,
}

impl PixelFormatChoice {
    fn spa(self) -> VideoFormat {
        match self {
            Self::Bgrx => VideoFormat::BGRx,
            Self::Bgra => VideoFormat::BGRA,
            Self::Rgbx => VideoFormat::RGBx,
            Self::Rgba => VideoFormat::RGBA,
        }
    }
    fn from_spa(f: VideoFormat) -> Option<Self> {
        [Self::Bgrx, Self::Bgra, Self::Rgbx, Self::Rgba]
            .into_iter()
            .find(|c| c.spa() == f)
    }
    fn has_alpha(self) -> bool {
        matches!(self, Self::Bgra | Self::Rgba)
    }
}

pub struct ProducerConfig {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub max_fps: u32,
}

enum Msg {
    Frame(Frame),
    Quit,
}

/// Handle to the PipeWire thread; also the pump's [`FrameSink`].
pub struct Producer {
    tx: pw::channel::Sender<Msg>,
    node_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl Producer {
    /// Spawn the pw thread and wait for the node id.
    pub fn spawn(cfg: ProducerConfig) -> Result<Self, String> {
        let (tx, rx) = pw::channel::channel::<Msg>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<u32, String>>();
        let thread = std::thread::Builder::new()
            .name("ecl-pw".into())
            .spawn(move || {
                if let Err(e) = run(cfg, rx, &ready_tx) {
                    let _ = ready_tx.send(Err(e.to_string()));
                }
            })
            .map_err(|e| e.to_string())?;
        let node_id = ready_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .map_err(|_| "pipewire did not publish a node".to_string())??;
        Ok(Self {
            tx,
            node_id,
            thread: Some(thread),
        })
    }

    /// The id apps use with `pipewire_stream_new` after `OpenPipeWireRemote`.
    pub fn node_id(&self) -> u32 {
        self.node_id
    }
}

impl FrameSink for Producer {
    fn push(&mut self, frame: Frame) -> Result<(), String> {
        self.tx
            .send(Msg::Frame(frame))
            .map_err(|_| "pipewire thread gone".to_string())
    }
}

impl Drop for Producer {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Production [`SinkFactory`]: one PipeWire producer node per stream.
pub struct PwSinkFactory;

impl crate::stream::SinkFactory for PwSinkFactory {
    fn make(&self, cfg: crate::stream::SinkConfig) -> Result<crate::stream::MadeSink, String> {
        let p = Producer::spawn(ProducerConfig {
            width: cfg.width,
            height: cfg.height,
            format: cfg.format,
            max_fps: cfg.max_fps,
        })?;
        let node_id = p.node_id();
        Ok(crate::stream::MadeSink {
            sink: Box::new(p),
            node_id,
        })
    }
}

fn pod_bytes(obj: pod::Object) -> Vec<u8> {
    PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(obj))
        .expect("serialize pod")
        .0
        .into_inner()
}

fn format_param(cfg: &ProducerConfig) -> Vec<u8> {
    let alts = advertised(cfg.format);
    let fmt_prop = Property {
        key: FormatProperties::VideoFormat.as_raw(),
        flags: PropertyFlags::empty(),
        value: Value::Choice(ChoiceValue::Id(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Enum {
                default: Id(alts[0].spa().as_raw()),
                alternatives: alts.iter().map(|a| Id(a.spa().as_raw())).collect(),
            },
        ))),
    };
    let max_fps = Property {
        key: FormatProperties::VideoMaxFramerate.as_raw(),
        flags: PropertyFlags::empty(),
        value: Value::Choice(ChoiceValue::Fraction(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Range {
                default: Fraction {
                    num: cfg.max_fps.max(1),
                    denom: 1,
                },
                min: Fraction { num: 1, denom: 1 },
                max: Fraction { num: 240, denom: 1 },
            },
        ))),
    };
    pod_bytes(pod::object!(
        SpaTypes::ObjectParamFormat,
        ParamType::EnumFormat,
        pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        fmt_prop,
        pod::property!(
            FormatProperties::VideoSize,
            Rectangle,
            Rectangle {
                width: cfg.width,
                height: cfg.height
            }
        ),
        pod::property!(
            FormatProperties::VideoFramerate,
            Fraction,
            Fraction { num: 0, denom: 1 }
        ),
        max_fps,
    ))
}

/// Buffers param: memfd, 3..8 buffers, one block, `stride * height`.
fn buffers_param(w: u32, h: u32) -> Vec<u8> {
    use spa::sys::*;
    let stride = (w * 4) as i32;
    let size = stride * h as i32;
    let range = Value::Choice(ChoiceValue::Int(Choice(
        ChoiceFlags::empty(),
        ChoiceEnum::Range {
            default: 4,
            min: 3,
            max: 8,
        },
    )));
    pod_bytes(pod::Object {
        type_: SpaTypes::ObjectParamBuffers.as_raw(),
        id: ParamType::Buffers.as_raw(),
        properties: vec![
            Property::new(SPA_PARAM_BUFFERS_buffers, range),
            Property::new(SPA_PARAM_BUFFERS_blocks, Value::Int(1)),
            Property::new(SPA_PARAM_BUFFERS_size, Value::Int(size)),
            Property::new(SPA_PARAM_BUFFERS_stride, Value::Int(stride)),
            Property::new(SPA_PARAM_BUFFERS_align, Value::Int(16)),
            Property::new(SPA_PARAM_BUFFERS_dataType, Value::Int(1 << SPA_DATA_MemFd)),
        ],
    })
}

struct Shared {
    latest: Option<Frame>,
    negotiated: Option<(PixelFormatChoice, u32, u32)>,
}

fn run(
    cfg: ProducerConfig,
    rx: pw::channel::Receiver<Msg>,
    ready: &mpsc::Sender<Result<u32, String>>,
) -> Result<(), pw::Error> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    let stream = pw::stream::StreamRc::new(
        core,
        NODE_NAME,
        properties! {
            *pw::keys::MEDIA_CLASS => "Video/Source",
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
            *pw::keys::NODE_NAME => "eclipse-screen-share",
            *pw::keys::NODE_DESCRIPTION => NODE_NAME,
        },
    )?;

    let shared = Rc::new(RefCell::new(Shared {
        latest: None,
        negotiated: None,
    }));
    let ready_ok = ready.clone();

    let _listener = stream
        .add_local_listener_with_user_data(shared.clone())
        .state_changed(move |st, _, _old, new| {
            if matches!(new, pw::stream::StreamState::Paused) {
                // Node exists once we leave Connecting.
                let _ = ready_ok.send(Ok(st.node_id()));
            }
        })
        .param_changed(move |st, sh, id, param| {
            let Some(param) = param else { return };
            if id != ParamType::Format.as_raw() {
                return;
            }
            let mut info = VideoInfoRaw::new();
            if info.parse(param).is_err() {
                return;
            }
            let Some(choice) = PixelFormatChoice::from_spa(info.format()) else {
                return;
            };
            let size = info.size();
            sh.borrow_mut().negotiated = Some((choice, size.width, size.height));
            let b = buffers_param(size.width, size.height);
            if let Some(p) = Pod::from_bytes(&b) {
                let _ = st.update_params(&mut [p]);
            }
        })
        .process(|st, sh| {
            let mut sh = sh.borrow_mut();
            let (Some(frame), Some((choice, ..))) = (sh.latest.take(), sh.negotiated) else {
                return;
            };
            let Some(mut buf) = st.dequeue_buffer() else {
                sh.latest = Some(frame); // consumer busy: keep newest
                return;
            };
            let datas = buf.datas_mut();
            let Some(d) = datas.first_mut() else { return };
            let stride = frame.width * 4;
            let written = d
                .data()
                .and_then(|dst| frame.copy_into(dst, stride, choice.has_alpha()));
            let c = d.chunk_mut();
            *c.offset_mut() = 0;
            *c.stride_mut() = stride as i32;
            *c.size_mut() = written.unwrap_or(0) as u32;
        })
        .register()?;

    let fmt = format_param(&cfg);
    let mut params = [Pod::from_bytes(&fmt).expect("format pod")];
    stream.connect(
        spa::utils::Direction::Output,
        None,
        pw::stream::StreamFlags::DRIVER
            | pw::stream::StreamFlags::TRIGGER
            | pw::stream::StreamFlags::ALLOC_BUFFERS
            | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;

    let ml = mainloop.clone();
    let st = stream.clone();
    let _rx = rx.attach(mainloop.loop_(), move |m| match m {
        Msg::Frame(f) => {
            shared.borrow_mut().latest = Some(f);
            // Damage-driven: cycle the graph now, do not wait to be pulled.
            let _ = st.trigger_process();
        }
        Msg::Quit => ml.quit(),
    });

    mainloop.run();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertised_keeps_channel_order() {
        assert_eq!(
            advertised(PixelFormat::Xrgb8888),
            [PixelFormatChoice::Bgrx, PixelFormatChoice::Bgra]
        );
        assert_eq!(
            advertised(PixelFormat::Abgr8888),
            [PixelFormatChoice::Rgbx, PixelFormatChoice::Rgba]
        );
    }
}
