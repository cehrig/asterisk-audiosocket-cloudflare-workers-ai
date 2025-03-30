use base64::prelude::BASE64_STANDARD;
use base64::Engine;
use std::cmp::min;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};
use std::ptr::slice_from_raw_parts;
use std::sync::{mpsc, Arc};
use std::thread::{scope, spawn};
use url::Url;

const MAX_SPEECH_MSEC: f64 = 60000.0;
const WAIT_MSEC: f64 = 750.0;
const CONSIDER_MSEC: f64 = 100.0;
const ACTIVE_ENERGY: i16 = 500;


pub struct Agent {
    stream: TcpStream,
}

struct CallBytes {
    data: Vec<u8>,
}

#[derive(Debug)]
enum CallTypes {
    Terminate,
    Uuid(Vec<u8>),
    Audio(Vec<u8>),
    Error,
}

#[derive(Copy, Clone, Debug)]
enum VadState {
    Silence,
    Consider(f64),
    Speech(f64),
    Wait(f64, f64),
}

struct Vad {
    rate: u32,
    bits: u8,
    data: Vec<u8>,
    pos: usize,
    state: VadState,
    start: Option<usize>,
}

struct Frame {
    inner: [u8; 2],
}

struct FrameIterator<'a> {
    inner: &'a Vad,
    pos: usize,
}

struct Chunk<'a> {
    inner: &'a [Frame],
}

struct Chunks<'a> {
    inner: &'a Vad,
    samples: usize,
    pos: usize,
}

trait Energy {
    fn energy(&self) -> i16;
}

impl Agent {
    pub fn new(stream: TcpStream) -> Self {
        Agent { stream }
    }

    fn clone_stream(&self) -> TcpStream {
        self.stream.try_clone().unwrap()
    }

    pub fn run(mut stream: TcpStream, url: &'static Url) {
        spawn(move || {
            let agent = Arc::new(Agent::new(stream.try_clone().unwrap()));
            let (in_tx, in_rx) = mpsc::channel();
            let (out_tx, out_rx) = mpsc::channel();
            let url = url;

            scope(|scope| {
                scope.spawn(|| {
                    Self::call_handler(Arc::clone(&agent), in_tx, out_rx)
                });
                scope.spawn(|| {
                    Self::ws_handler(Arc::clone(&agent), in_rx, out_tx, url)
                });
            });

            let _ = stream
                .write(CallTypes::Terminate.to_vec().as_slice())
                .unwrap();

            stream.shutdown(std::net::Shutdown::Both).unwrap();
        });
    }

    fn ws_handler(
        self: Arc<Self>,
        rx: mpsc::Receiver<Vec<u8>>,
        tx: mpsc::Sender<Vec<u8>>,
        url: &Url,
    ) {
        let client = reqwest::blocking::Client::new();

        while let Ok(wave) = rx.recv() {
            let status = client
                .post(url.clone())
                .body(wave)
                .send()
                .unwrap();

            let text = status.text().unwrap();

            let mp3_data = BASE64_STANDARD
                .decode(text)
                .expect("Failed to decode base64");

            let mut child = Command::new("ffmpeg")
                .arg("-i")
                .arg("pipe:0")
                .arg("-f")
                .arg("s16le")
                .arg("-ac")
                .arg("1")
                .arg("-acodec")
                .arg("pcm_s16le")
                .arg("-ar")
                .arg("8000")
                .arg("pipe:1")
                .stdin(Stdio::piped())
                .stderr(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();

            let mut stdin = child.stdin.take().unwrap();
            let mut stdout = child.stdout.take().unwrap();

            let (input, output) = {
                (
                    spawn(move || {
                        stdin.write_all(&mp3_data).unwrap();
                    }),
                    spawn(move || {
                        let mut output = Vec::new();
                        loop {
                            let mut buf = [0u8; 1024];
                            let n = stdout.read(&mut buf).unwrap();

                            if n == 0 {
                                return output;
                            }

                            output.extend_from_slice(&buf[..n]);
                        }
                    }),
                )
            };

            input.join().unwrap();

            let output = output.join().unwrap();
            let _ = child.wait().unwrap();

            tx.send(output).unwrap();
        }
    }

    fn call_handler(
        self: Arc<Self>,
        tx: mpsc::Sender<Vec<u8>>,
        rx: mpsc::Receiver<Vec<u8>>,
    ) {
        let mut stream = self.clone_stream();
        let mut wav = Vad::new(8000, 16);
        let mut relay = None;
        let mut blocked = false;
        let mut data = CallBytes::new();
        let mut raw;


        loop {
            raw = [0u8; 1024];

            let Ok(bytes) = stream.read(&mut raw) else {
                break;
            };

            if bytes == 0 {
                break;
            }

            if let Ok(out) = rx.try_recv() {
                relay = Some(out);
            }

            let extracted = data.extract(Some(&raw[0..bytes]));

            for kind in extracted {
                let CallTypes::Audio(audio) = kind else {
                    continue;
                };

                if relay.is_some() {
                    let response = {
                        let response = relay.as_mut().unwrap();

                        if response.is_empty() {
                            relay = None;
                            blocked = false;

                            continue;
                        }

                        response
                    };

                    let _ = stream
                        .write(
                            CallTypes::Audio(
                                response
                                    .drain(..min(audio.len(), response.len()))
                                    .collect(),
                            )
                                .to_vec()
                                .as_slice(),
                        )
                        .unwrap();

                    continue;
                }

                if blocked {
                    continue;
                }

                if let Some(data) = wav.append(audio.as_slice()) {
                    blocked = true;
                    tx.send(wav.build(data).unwrap()).unwrap()
                }
            }
        }
    }
}

impl CallBytes {
    fn new() -> Self {
        CallBytes { data: Vec::new() }
    }

    fn add(&mut self, data: &[u8]) {
        self.data.extend_from_slice(data);
    }

    fn len(&self) -> usize {
        self.data.len()
    }

    fn extract(&mut self, new: Option<&[u8]>) -> Vec<CallTypes> {
        if let Some(data) = new {
            self.add(data)
        }

        let mut types = Vec::new();
        loop {
            // We need at least three bytes of packet data
            if self.len() < 3 {
                break;
            }

            // Payload length
            let len = u16::from_be_bytes([self.data[1], self.data[2]]) as usize;

            // We need at least as many packets as payload length
            if self.len() < len + 3 {
                break;
            }

            // Remove header and payload
            let data: Vec<u8> = self.data.drain(0..len + 3).collect();

            // Add packet
            types.push(CallTypes::from(data));
        }

        types
    }
}

impl CallTypes {
    fn id(&self) -> u8 {
        match self {
            CallTypes::Terminate => 0x00,
            CallTypes::Uuid(_) => 0x01,
            CallTypes::Audio(_) => 0x10,
            CallTypes::Error => 0xFF,
        }
    }

    fn to_vec(&self) -> Vec<u8> {
        match self {
            CallTypes::Terminate | CallTypes::Error => vec![self.id()],
            CallTypes::Uuid(data) | CallTypes::Audio(data) => {
                let mut vec = Vec::with_capacity(self.len() as usize + 3);
                vec.extend_from_slice(&[self.id()]);
                vec.extend_from_slice(self.len().to_be_bytes().as_slice());
                vec.extend_from_slice(data.as_slice());

                vec
            }
        }
    }

    fn len(&self) -> u16 {
        match self {
            CallTypes::Uuid(data) | CallTypes::Audio(data) => data.len() as u16,
            _ => 0,
        }
    }
}

impl From<Vec<u8>> for CallTypes {
    fn from(mut data: Vec<u8>) -> Self {
        match data[0] {
            0x00 => CallTypes::Terminate,
            0x01 => CallTypes::Uuid(data.drain(3..).collect()),
            0x10 => CallTypes::Audio(data.drain(3..).collect()),
            0xFF => CallTypes::Error,
            _ => unreachable!("unexpected call type"),
        }
    }
}

impl Vad {
    fn new(rate: u32, bits: u8) -> Self {
        Vad {
            rate,
            bits,
            data: Vec::new(),
            pos: 0,
            state: VadState::new(),
            start: None,
        }
    }

    fn sample_size(&self) -> usize {
        self.bits as usize / 8
    }

    fn len(&self) -> usize {
        self.data.len()
    }

    fn samples(&self) -> usize {
        self.len() / self.sample_size()
    }

    fn chunks(&self, samples: usize) -> Chunks {
        Chunks::new(self, samples, self.pos)
    }

    fn append(&mut self, data: &[u8]) -> Option<Vec<u8>> {
        self.data.extend_from_slice(data);

        let mut state = self.state;
        let mut n = 0;

        for chunk in self.chunks(20) {
            if chunk.energy() > ACTIVE_ENERGY {
                state.active(CONSIDER_MSEC, chunk.msec(self.rate));
            } else {
                state.silence(chunk.msec(self.rate));
            }

            n += chunk.samples();
        }

        if self.start.is_none() && state.is_recording() {
            self.start = Some(self.pos);
        }

        if state.is_ready(WAIT_MSEC) {
            let data = self.data.drain(self.start.unwrap()..).collect();

            self.state = VadState::new();
            self.pos = 0;
            self.start = None;

            return Some(data);
        }

        self.state = state;
        self.pos += n;

        None
    }

    fn build(&mut self, data: Vec<u8>) -> Option<Vec<u8>> {
        if data.is_empty() {
            return None;
        }

        let mut wave: Vec<u8> = Vec::new();

        // RIFF
        wave.extend_from_slice(&[0x52, 0x49, 0x46, 0x46]);

        // Filesize
        wave.extend_from_slice(&((data.len() + 36) as u32).to_le_bytes());

        // WAVE
        wave.extend_from_slice(&[0x57, 0x41, 0x56, 0x45]);

        // FMT
        wave.extend_from_slice(&[0x66, 0x6d, 0x74, 0x20]);

        // BlocSize
        wave.extend_from_slice(&[0x10, 0x00, 0x00, 0x00]);

        // PCM
        wave.extend_from_slice(&[0x01, 0x00]);

        // NbrChannels
        wave.extend_from_slice(&[0x01, 0x00]);

        // Frequency
        wave.extend_from_slice(&self.rate.to_le_bytes());

        // Byte per sec
        wave.extend_from_slice(
            &(self.rate * (self.bits / 8) as u32).to_le_bytes(),
        );

        // Byte per Bloc
        wave.extend_from_slice(&((self.bits / 8) as u16).to_le_bytes());

        // Bits per Sample
        wave.extend_from_slice(&(self.bits as u16).to_le_bytes());

        // data
        wave.extend_from_slice(&[0x64, 0x61, 0x74, 0x61]);

        // Data Size
        wave.extend_from_slice(&(data.len() as u32).to_le_bytes());

        // Sampled Data
        wave.extend_from_slice(&data);

        Some(wave)
    }
}

impl<'a> IntoIterator for &'a Vad {
    type Item = Frame;
    type IntoIter = FrameIterator<'a>;

    fn into_iter(self) -> Self::IntoIter {
        FrameIterator {
            inner: self,
            pos: self.pos,
        }
    }
}

impl VadState {
    fn new() -> Self {
        VadState::Silence
    }

    fn active(&mut self, consider: f64, msec: f64) {
        *self = match self {
            VadState::Silence => VadState::Consider(msec),
            VadState::Consider(s) => {
                if *s >= consider {
                    VadState::Speech(*s + msec)
                } else {
                    VadState::Consider(*s + msec)
                }
            }
            VadState::Speech(s) => VadState::Speech(*s + msec),
            VadState::Wait(t, s) => VadState::Speech(*t + *s + msec),
        }
    }

    fn silence(&mut self, msec: f64) {
        *self = match self {
            VadState::Silence => VadState::Silence,
            VadState::Consider(_) => VadState::Silence,
            VadState::Speech(s) => VadState::Wait(*s, msec),
            VadState::Wait(t, s) => VadState::Wait(*t, *s + msec),
        }
    }

    fn is_recording(&self) -> bool {
        matches!(self, VadState::Speech(_) | VadState::Wait(_, _))
    }

    fn is_ready(&self, msec: f64) -> bool {
        match self {
            VadState::Speech(s) if *s >= MAX_SPEECH_MSEC => true,
            VadState::Wait(t, s) if *t >= MAX_SPEECH_MSEC || *s >= msec => true,
            _ => false,
        }
    }
}

impl Frame {
    fn new(inner: [u8; 2]) -> Self {
        Frame { inner }
    }
}

impl Energy for Frame {
    fn energy(&self) -> i16 {
        i16::from_le_bytes(self.inner).abs()
    }
}

impl<'a> Iterator for FrameIterator<'a> {
    type Item = Frame;

    fn next(&mut self) -> Option<Self::Item> {
        // We are out of samples
        if self.inner.samples() - self.pos == 0 {
            return None;
        }

        let start = self.pos * self.inner.sample_size();

        let frame =
            Frame::new([self.inner.data[start], self.inner.data[start + 1]]);

        self.pos += 1;

        Some(frame)
    }
}

impl<'a> Chunk<'a> {
    unsafe fn from_raw_ptr<T>(ptr: *const T, len: usize) -> Self {
        Chunk {
            inner: unsafe {
                &*(slice_from_raw_parts::<Frame>(ptr as *const Frame, len))
            },
        }
    }

    fn samples(&self) -> usize {
        self.inner.len()
    }

    fn msec(&self, rate: u32) -> f64 {
        self.samples() as f64 * 1000.0 / rate as f64
    }
}


impl<'a> Energy for Chunk<'a> {
    fn energy(&self) -> i16 {
        self.inner
            .iter()
            .max_by_key(|f| f.energy())
            .unwrap()
            .energy()
    }
}

impl<'a> Chunks<'a> {
    fn new(inner: &'a Vad, samples: usize, pos: usize) -> Self {
        Chunks {
            inner,
            samples,
            pos,
        }
    }
}

impl<'a> Iterator for Chunks<'a> {
    type Item = Chunk<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos + self.samples > self.inner.samples() {
            return None;
        }

        let chunk = unsafe {
            Chunk::from_raw_ptr(
                self.inner
                    .data
                    .as_ptr()
                    .add(self.pos * self.inner.sample_size()),
                self.samples,
            )
        };

        self.pos += self.samples;

        Some(chunk)
    }
}


