//! Reads "now playing" info from Windows' Global System Media Transport Controls.
//!
//! Runs on its own small thread and is event driven: Windows tells us when the
//! track, playback state or timeline changes, so there is no busy polling. A
//! slow safety refresh (every few seconds) catches apps that forget to notify.

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    sync::{mpsc, Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use windows::{
    Foundation::TypedEventHandler,
    Media::Control::*,
    Storage::Streams::{Buffer, DataReader, InputStreamOptions},
    Win32::Foundation::{HWND, LPARAM, WPARAM},
    Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED},
    Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime,
    Win32::UI::WindowsAndMessaging::PostMessageW,
};

type Session = GlobalSystemMediaTransportControlsSession;
type Manager = GlobalSystemMediaTransportControlsSessionManager;

#[derive(Clone, Default)]
pub struct Track {
    pub active: bool,
    pub title: String,
    pub artist: String,
    pub playing: bool,
    /// Position in seconds at the moment `at` was captured.
    pub pos: f64,
    pub dur: f64,
    pub at: Option<Instant>,
    pub key: u64,
    pub thumb: Option<Arc<Vec<u8>>>,
    pub thumb_id: u64,
}

impl Track {
    pub fn position(&self) -> f64 {
        let mut p = self.pos;
        if self.playing {
            if let Some(at) = self.at {
                p += at.elapsed().as_secs_f64();
            }
        }
        if self.dur > 0.0 {
            p.clamp(0.0, self.dur)
        } else {
            p.max(0.0)
        }
    }
}

#[derive(Clone, Copy)]
pub enum Cmd {
    Toggle,
    Next,
    Prev,
}

enum Msg {
    Refresh,
    Resubscribe,
    Cmd(Cmd),
}

pub struct Media {
    pub shared: Arc<Mutex<Track>>,
    tx: mpsc::Sender<Msg>,
}

impl Media {
    pub fn command(&self, c: Cmd) {
        let _ = self.tx.send(Msg::Cmd(c));
    }
}

/// Starts the media thread. `msg` is posted to `hwnd` whenever the track changes.
pub fn start(hwnd: HWND, msg: u32) -> Media {
    let shared = Arc::new(Mutex::new(Track::default()));
    let (tx, rx) = mpsc::channel();
    let raw = hwnd.0 as isize;
    let (s2, tx2) = (shared.clone(), tx.clone());
    let _ =
        thread::Builder::new().name("media".into()).stack_size(512 * 1024).spawn(move || worker(raw, msg, s2, tx2, rx));
    Media { shared, tx }
}

struct Sub {
    s: Session,
    tokens: [i64; 3],
}

/// How long after a track change we keep re-checking the cover art. Players
/// (Spotify especially) publish the new title first and the new art later.
const ART_SETTLE: Duration = Duration::from_secs(8);

struct State {
    last_key: u64,
    thumb: Option<Arc<Vec<u8>>>,
    /// Art of the previous track, so a stale copy isn't shown for the new one.
    prev_thumb: Option<Arc<Vec<u8>>>,
    thumb_id: u64,
    changed_at: Instant,
}

impl Default for State {
    fn default() -> Self {
        Self { last_key: 0, thumb: None, prev_thumb: None, thumb_id: 0, changed_at: Instant::now() - ART_SETTLE }
    }
}

impl State {
    fn settling(&self) -> bool {
        self.changed_at.elapsed() < ART_SETTLE
    }
}

fn worker(raw: isize, msg: u32, shared: Arc<Mutex<Track>>, tx: mpsc::Sender<Msg>, rx: mpsc::Receiver<Msg>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let notify = || unsafe {
        let _ = PostMessageW(Some(HWND(raw as *mut _)), msg, WPARAM(0), LPARAM(0));
    };

    let mgr = loop {
        match Manager::RequestAsync().and_then(|op| op.join()) {
            Ok(m) => break m,
            Err(_) => thread::sleep(Duration::from_secs(3)),
        }
    };
    {
        let tx = tx.clone();
        let _ = mgr.CurrentSessionChanged(&TypedEventHandler::new(move |_, _| {
            let _ = tx.send(Msg::Resubscribe);
            Ok(())
        }));
    }

    let mut sub: Option<Sub> = None;
    let mut st = State::default();
    resubscribe(&mgr, &mut sub, &tx);
    refresh(&sub, &mut st, &shared, &notify);

    loop {
        // Check back quickly while new cover art may still be on its way.
        let wait = if st.settling() { Duration::from_millis(700) } else { Duration::from_secs(4) };
        match rx.recv_timeout(wait) {
            Ok(Msg::Cmd(c)) => run_cmd(&sub, c),
            Ok(Msg::Resubscribe) => resubscribe(&mgr, &mut sub, &tx),
            Ok(Msg::Refresh) => {
                // Apps often fire several events at once - coalesce them.
                thread::sleep(Duration::from_millis(40));
                while let Ok(m) = rx.try_recv() {
                    match m {
                        Msg::Cmd(c) => run_cmd(&sub, c),
                        Msg::Resubscribe => resubscribe(&mgr, &mut sub, &tx),
                        Msg::Refresh => {}
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => break,
        }
        refresh(&sub, &mut st, &shared, &notify);
    }
}

fn run_cmd(sub: &Option<Sub>, c: Cmd) {
    let Some(sub) = sub else { return };
    let s = &sub.s;
    let _ = match c {
        Cmd::Toggle => s.TryTogglePlayPauseAsync(),
        Cmd::Next => s.TrySkipNextAsync(),
        Cmd::Prev => s.TrySkipPreviousAsync(),
    }
    .and_then(|op| op.join());
}

fn resubscribe(mgr: &Manager, sub: &mut Option<Sub>, tx: &mpsc::Sender<Msg>) {
    if let Some(old) = sub.take() {
        let _ = old.s.RemoveMediaPropertiesChanged(old.tokens[0]);
        let _ = old.s.RemovePlaybackInfoChanged(old.tokens[1]);
        let _ = old.s.RemoveTimelinePropertiesChanged(old.tokens[2]);
    }
    let Ok(s) = mgr.GetCurrentSession() else { return };
    macro_rules! handler {
        () => {{
            let tx = tx.clone();
            TypedEventHandler::new(move |_, _| {
                let _ = tx.send(Msg::Refresh);
                Ok(())
            })
        }};
    }
    let t0 = s.MediaPropertiesChanged(&handler!()).unwrap_or(0);
    let t1 = s.PlaybackInfoChanged(&handler!()).unwrap_or(0);
    let t2 = s.TimelinePropertiesChanged(&handler!()).unwrap_or(0);
    *sub = Some(Sub { s, tokens: [t0, t1, t2] });
}

fn refresh(sub: &Option<Sub>, st: &mut State, shared: &Arc<Mutex<Track>>, notify: &dyn Fn()) {
    let new = sub.as_ref().and_then(|s| read(&s.s, st)).unwrap_or_default();
    let mut cur = shared.lock().unwrap();
    let changed = cur.active != new.active
        || cur.key != new.key
        || cur.playing != new.playing
        || cur.thumb_id != new.thumb_id
        || (cur.dur - new.dur).abs() > 0.5
        || (cur.position() - new.position()).abs() > 1.5;
    if changed {
        *cur = new;
        drop(cur);
        notify();
    }
}

fn read(s: &Session, st: &mut State) -> Option<Track> {
    let props = s.TryGetMediaPropertiesAsync().ok()?.join().ok()?;
    let title = props.Title().ok()?.to_string();
    if title.is_empty() {
        return None;
    }
    let mut artist = props.Artist().map(|h| h.to_string()).unwrap_or_default();
    if artist.is_empty() {
        artist = props.AlbumArtist().map(|h| h.to_string()).unwrap_or_default();
    }
    let app = s.SourceAppUserModelId().map(|h| h.to_string()).unwrap_or_default();

    let mut h = DefaultHasher::new();
    (&app, &title, &artist).hash(&mut h);
    let key = h.finish();
    if key != st.last_key {
        st.last_key = key;
        st.prev_thumb = st.thumb.take().or(st.prev_thumb.take());
        st.changed_at = Instant::now();
    }
    // Right after a change, keep re-reading the art: the first copy is often
    // still the previous song's. Afterwards, only read it if we have none.
    let settling = st.settling();
    if settling || st.thumb.is_none() {
        if let Some(b) = read_thumb(&props) {
            let same_as_current = st.thumb.as_ref().is_some_and(|t| **t == b);
            // Two songs from the same album share art, so a match with the
            // previous track is only trusted once the settle window is over.
            let stale = settling && st.prev_thumb.as_ref().is_some_and(|t| **t == b);
            if !same_as_current && !stale {
                st.thumb_id += 1;
                st.thumb = Some(Arc::new(b));
            }
        }
    }

    let playing = s
        .GetPlaybackInfo()
        .and_then(|i| i.PlaybackStatus())
        .map(|p| p == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing)
        .unwrap_or(false);

    let (mut pos, mut dur) = (0.0, 0.0);
    if let Ok(tl) = s.GetTimelineProperties() {
        let secs = |d: windows::Foundation::TimeSpan| d.Duration as f64 / 1e7;
        let start = tl.StartTime().map(secs).unwrap_or(0.0);
        dur = tl.EndTime().map(secs).unwrap_or(0.0) - start;
        pos = tl.Position().map(secs).unwrap_or(0.0);
        if playing && dur > 0.0 {
            if let Ok(lu) = tl.LastUpdatedTime() {
                let ft = unsafe { GetSystemTimePreciseAsFileTime() };
                let now = ((ft.dwHighDateTime as i64) << 32) | ft.dwLowDateTime as i64;
                let delta = (now - lu.UniversalTime) as f64 / 1e7;
                if lu.UniversalTime > 0 && (0.0..86_400.0).contains(&delta) {
                    pos += delta;
                }
            }
        }
        if dur <= 0.0 {
            dur = 0.0;
            pos = 0.0;
        }
    }

    Some(Track {
        active: true,
        title,
        artist,
        playing,
        pos,
        dur,
        at: Some(Instant::now()),
        key,
        thumb_id: if st.thumb.is_some() { st.thumb_id } else { 0 },
        thumb: st.thumb.clone(),
    })
}

fn read_thumb(props: &GlobalSystemMediaTransportControlsSessionMediaProperties) -> Option<Vec<u8>> {
    let stream = props.Thumbnail().ok()?.OpenReadAsync().ok()?.join().ok()?;
    let size = stream.Size().ok()?;
    if size == 0 || size > 16 * 1024 * 1024 {
        return None;
    }
    let size = size as u32;
    let buf = Buffer::Create(size).ok()?;
    let ib = stream.ReadAsync(&buf, size, InputStreamOptions::ReadAhead).ok()?.join().ok()?;
    let n = ib.Length().ok()? as usize;
    let reader = DataReader::FromBuffer(&ib).ok()?;
    let mut v = vec![0u8; n];
    reader.ReadBytes(&mut v).ok()?;
    Some(v)
}
