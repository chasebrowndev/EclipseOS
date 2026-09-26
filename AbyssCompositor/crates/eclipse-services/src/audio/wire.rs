// SPDX-License-Identifier: AGPL-3.0-only
//! One PulseAudio native-protocol connection, spoken synchronously.
//!
//! Every message is read whole into a reused frame buffer before it is
//! parsed, so a reply the `pulseaudio` crate reads short (a newer server
//! appending a field) cannot desynchronise the stream.

use std::ffi::CStr;
use std::io::{BufReader, Cursor, Read};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use pulseaudio::protocol::{
    self, AuthParams, AuthReply, Command, CommandReply, Descriptor, Prop, Props, ProtocolError,
    SetClientNameReply, DESCRIPTOR_SIZE, MAX_MEMBLOCKQ_LENGTH,
};

pub(super) type Result<T> = std::result::Result<T, ProtocolError>;

/// How long a request may wait for its reply before the connection is
/// treated as dead.
const REPLY_TIMEOUT: Duration = Duration::from_secs(3);

pub(super) struct Conn {
    sock: BufReader<UnixStream>,
    version: u16,
    seq: u32,
    frame: Vec<u8>,
}

/// Connect, authenticate and name the client.
pub(super) fn connect(path: &Path, name: &CStr) -> Result<Conn> {
    let stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(REPLY_TIMEOUT))?;
    stream.set_write_timeout(Some(REPLY_TIMEOUT))?;
    let mut conn = Conn {
        sock: BufReader::new(stream),
        version: protocol::MAX_VERSION,
        seq: 0,
        frame: Vec::new(),
    };
    // pipewire-pulse ignores the cookie; a real PulseAudio wants it.
    let cookie = pulseaudio::cookie_path_from_env()
        .and_then(|path| std::fs::read(path).ok())
        .unwrap_or_default();
    let auth: AuthReply = conn.request(&Command::Auth(AuthParams {
        version: protocol::MAX_VERSION,
        supports_shm: false,
        supports_memfd: false,
        cookie,
    }))?;
    conn.version = auth.version.min(protocol::MAX_VERSION);
    let mut props = Props::new();
    props.set(Prop::ApplicationName, name);
    let _: SetClientNameReply = conn.request(&Command::SetClientName(props))?;
    Ok(conn)
}

impl Conn {
    pub(super) fn stream(&self) -> &UnixStream {
        self.sock.get_ref()
    }

    fn send(&mut self, command: &Command) -> Result<u32> {
        let seq = self.seq;
        self.seq = self.seq.wrapping_add(1);
        protocol::write_command_message(self.sock.get_mut(), seq, command, self.version)?;
        Ok(seq)
    }

    /// Send `command` and read its typed reply.
    pub(super) fn request<T: CommandReply>(&mut self, command: &Command) -> Result<T> {
        let seq = self.send(command)?;
        loop {
            if self.read_frame()?.channel != u32::MAX {
                continue; // stream data is not a reply
            }
            let (got, reply) =
                protocol::read_reply_message::<T>(&mut Cursor::new(&self.frame[..]), self.version)?;
            return check_seq(seq, got).map(|()| reply);
        }
    }

    /// Send `command` and wait for its empty acknowledgement.
    pub(super) fn ack(&mut self, command: &Command) -> Result<()> {
        let seq = self.send(command)?;
        loop {
            if self.read_frame()?.channel != u32::MAX {
                continue;
            }
            let got = protocol::read_ack_message(&mut Cursor::new(&self.frame[..]))?;
            return check_seq(seq, got);
        }
    }

    /// Read the next unsolicited command (an event), skipping stream data.
    pub(super) fn read_command(&mut self) -> Result<Command> {
        loop {
            if self.read_frame()?.channel != u32::MAX {
                continue;
            }
            let (_, command) = Command::read_tag_prefixed(&mut Cursor::new(self.payload()), self.version)?;
            return Ok(command);
        }
    }

    /// Read one whole message into the frame buffer.
    pub(super) fn read_frame(&mut self) -> Result<Descriptor> {
        self.frame.resize(DESCRIPTOR_SIZE, 0);
        self.sock.read_exact(&mut self.frame)?;
        let desc = protocol::read_descriptor(&mut &self.frame[..])?;
        let len = desc.length as usize;
        if len > MAX_MEMBLOCKQ_LENGTH {
            return Err(ProtocolError::Invalid("oversized message".into()));
        }
        self.frame.resize(DESCRIPTOR_SIZE + len, 0);
        self.sock.read_exact(&mut self.frame[DESCRIPTOR_SIZE..])?;
        Ok(desc)
    }

    /// The payload of the last frame read.
    pub(super) fn payload(&self) -> &[u8] {
        &self.frame[DESCRIPTOR_SIZE..]
    }

    pub(super) fn version(&self) -> u16 {
        self.version
    }
}

fn check_seq(sent: u32, got: u32) -> Result<()> {
    if sent == got {
        Ok(())
    } else {
        Err(ProtocolError::Invalid("reply out of sequence".into()))
    }
}
