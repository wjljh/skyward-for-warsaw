use anyhow::{anyhow, Context, Result};
use evdev::raw_stream::RawDevice;
use evdev::{AbsoluteAxisCode, EventSummary, InputEvent, KeyCode, PropType, SynchronizationCode};
use skyward_config::Write;
use skyward_sys::sleep::Wakeful;
use skyward_ui::app::{Key, Touch};
use std::collections::HashSet;
use std::ffi::CString;
use std::fs;
use std::io::{self, ErrorKind};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub enum Event {
    /// A touch and when the panel reported it, on [`monotonic_ms`]'s clock.
    Touch(Touch, u64),
    /// A button going down, or coming back up.
    Key(Key, bool),
}

/// Milliseconds on the monotonic clock, which the touch panel stamps its
/// events with.
pub fn monotonic_ms() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    assert_eq!(rc, 0, "read the monotonic clock");
    ts.tv_sec as u64 * 1000 + ts.tv_nsec as u64 / 1_000_000
}

fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

const KEYS: [KeyCode; 4] = [KeyCode::KEY_POWER, KeyCode::KEY_VOLUMEUP, KeyCode::KEY_VOLUMEDOWN, KeyCode::KEY_BACK];

fn key_of(code: KeyCode) -> Option<Key> {
    match code {
        KeyCode::KEY_POWER => Some(Key::Power),
        KeyCode::KEY_VOLUMEUP => Some(Key::VolumeUp),
        KeyCode::KEY_VOLUMEDOWN => Some(Key::VolumeDown),
        KeyCode::KEY_BACK => Some(Key::Back),
        _ => None,
    }
}

/// A device read failed because it was unplugged.
fn gone(e: &io::Error) -> bool {
    e.raw_os_error() == Some(libc::ENODEV)
}

/// Slot 0's value of a multitouch axis. The kernel keeps multitouch values
/// per slot, apart from the axis state the ABS queries return, and hands
/// them out through EVIOCGMTSLOTS: the axis code, then one value per slot.
fn slot0_value(dev: &RawDevice, axis: AbsoluteAxisCode, slots: usize) -> io::Result<i32> {
    let mut buf = vec![0i32; 1 + slots];
    buf[0] = i32::from(axis.0);
    // _IOC(_IOC_READ, 'E', 0x0a, size)
    let request = (2u32 << 30) | (((buf.len() * 4) as u32) << 16) | (u32::from(b'E') << 8) | 0x0a;
    if unsafe { libc::ioctl(dev.as_raw_fd(), request as _, buf.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(buf[1])
}

/// The direct-touch panel. Only the first finger is followed (multitouch
/// protocol B, slot 0).
struct Panel {
    dev: RawDevice,
    path: PathBuf,
    x_range: (i32, i32),
    y_range: (i32, i32),
    slots: usize,
    slot: i32,
    down: bool,
    was_down: bool,
    raw: (i32, i32),
    moved: bool,
    /// The kernel dropped events that were not read in time; what arrives
    /// up to the next report is incomplete.
    dropped: bool,
}

impl Panel {
    fn new(path: PathBuf, dev: RawDevice) -> Result<Self> {
        let mut x_range = None;
        let mut y_range = None;
        let mut slots = 1;
        for (axis, info) in dev.get_absinfo().context("read touch axis ranges")? {
            match axis {
                AbsoluteAxisCode::ABS_MT_POSITION_X => x_range = Some((info.minimum(), info.maximum())),
                AbsoluteAxisCode::ABS_MT_POSITION_Y => y_range = Some((info.minimum(), info.maximum())),
                AbsoluteAxisCode::ABS_MT_SLOT => slots = info.maximum() as usize + 1,
                _ => {}
            }
        }
        let x_range = x_range.ok_or_else(|| anyhow!("touch panel reports no X range"))?;
        let y_range = y_range.ok_or_else(|| anyhow!("touch panel reports no Y range"))?;
        // Stamp events with the clock the UI keeps time by: how fast a
        // finger moved when it lifted is measured from these stamps.
        // EVIOCSCLOCKID: _IOW('E', 0xa0, int)
        let request = (1u32 << 30) | (4u32 << 16) | (u32::from(b'E') << 8) | 0xa0;
        let clock: libc::c_int = libc::CLOCK_MONOTONIC;
        if unsafe { libc::ioctl(dev.as_raw_fd(), request as _, &clock) } < 0 {
            return Err(io::Error::last_os_error()).context("stamp touch events with the monotonic clock");
        }
        set_nonblocking(dev.as_raw_fd()).context("make touch non-blocking")?;
        let mut panel =
            Panel { dev, path, x_range, y_range, slots, slot: 0, down: false, was_down: false, raw: (0, 0), moved: false, dropped: false };
        // The kernel drops a position equal to the one a slot already holds,
        // so a touch landing where the last one lifted carries no position;
        // start from what the panel holds now.
        panel.resync().context("read the touch state")?;
        eprintln!("touch {:?}: x {:?} y {:?}, {slots} slots, slot 0 at {:?}", panel.dev.name().unwrap_or("?"), x_range, y_range, panel.raw);
        Ok(panel)
    }

    /// Read the current slot and slot 0's finger back from the panel.
    fn resync(&mut self) -> io::Result<()> {
        self.slot = self.dev.get_abs_state()?[usize::from(AbsoluteAxisCode::ABS_MT_SLOT.0)].value;
        self.down = slot0_value(&self.dev, AbsoluteAxisCode::ABS_MT_TRACKING_ID, self.slots)? >= 0;
        self.raw = (
            slot0_value(&self.dev, AbsoluteAxisCode::ABS_MT_POSITION_X, self.slots)?,
            slot0_value(&self.dev, AbsoluteAxisCode::ABS_MT_POSITION_Y, self.slots)?,
        );
        self.moved = true;
        Ok(())
    }

    fn read(&mut self, w: f32, h: f32, out: &mut Vec<Event>) -> io::Result<()> {
        let events: Vec<InputEvent> = match self.dev.fetch_events() {
            Ok(events) => events.collect(),
            Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(()),
            Err(e) => return Err(e),
        };
        for e in events {
            match e.destructure() {
                // Skip to the next report and read the state back there.
                EventSummary::Synchronization(_, SynchronizationCode::SYN_DROPPED, _) => self.dropped = true,
                EventSummary::Synchronization(_, SynchronizationCode::SYN_REPORT, _) => {
                    if std::mem::take(&mut self.dropped) {
                        self.resync()?;
                    }
                    let time = e.as_ref().time;
                    let at = time.tv_sec as u64 * 1000 + time.tv_usec as u64 / 1000;
                    let span = |r: (i32, i32)| (r.1 - r.0).max(1) as f32;
                    let x = (self.raw.0 - self.x_range.0) as f32 / span(self.x_range) * w;
                    let y = (self.raw.1 - self.y_range.0) as f32 / span(self.y_range) * h;
                    match (self.was_down, self.down) {
                        (false, true) => out.push(Event::Touch(Touch::Down(x, y), at)),
                        (true, true) if self.moved => out.push(Event::Touch(Touch::Move(x, y), at)),
                        (true, false) => out.push(Event::Touch(Touch::Up, at)),
                        _ => {}
                    }
                    self.was_down = self.down;
                    self.moved = false;
                }
                _ if self.dropped => {}
                EventSummary::AbsoluteAxis(_, AbsoluteAxisCode::ABS_MT_SLOT, v) => self.slot = v,
                EventSummary::AbsoluteAxis(_, AbsoluteAxisCode::ABS_MT_TRACKING_ID, v) if self.slot == 0 => self.down = v >= 0,
                EventSummary::AbsoluteAxis(_, AbsoluteAxisCode::ABS_MT_POSITION_X, v) if self.slot == 0 => {
                    self.raw.0 = v;
                    self.moved = true;
                }
                EventSummary::AbsoluteAxis(_, AbsoluteAxisCode::ABS_MT_POSITION_Y, v) if self.slot == 0 => {
                    self.raw.1 = v;
                    self.moved = true;
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// The touchscreen and the hardware buttons. /dev/input is watched, so a
/// device that shows up later, such as a touch panel whose driver module
/// loads after the UI starts, is picked up when it appears.
/// How many times the touchscreen's interrupt has been raised, as the kernel
/// counts it: it rises while a finger is on the panel, whether or not the
/// driver hands the touch over.
fn touch_interrupts() -> u64 {
    let Ok(text) = fs::read_to_string("/proc/interrupts") else {
        return 0;
    };
    let mut total = 0;
    for line in text.lines() {
        let name = line.to_ascii_lowercase();
        if !(name.contains("goodix") || name.contains("touch")) {
            continue;
        }
        for field in line.split_whitespace().skip(1) {
            match field.parse::<u64>() {
                Ok(count) => total += count,
                Err(_) => break,
            }
        }
    }
    total
}

pub struct Inputs {
    watch: OwnedFd,
    /// The watch and every device taken in: a touch or a key press keeps
    /// the phone awake until it is read.
    wakeful: Wakeful,
    panel: Option<Panel>,
    buttons: Vec<(PathBuf, RawDevice)>,
    seen: HashSet<PathBuf>,
    /// What the touchscreen is told each time its panel is taken in.
    touch_writes: Vec<Write>,
    /// How many times the touchscreen's interrupt has been raised, as the
    /// kernel counts it, and when it was last raised with no event following.
    touch_irqs: u64,
    raised_at: Option<Instant>,
    /// When the touchscreen last handed over an event, and when its driver was
    /// last loaded afresh.
    touched_at: Instant,
    reloaded_at: Option<Instant>,
    w: f32,
    h: f32,
}

impl Inputs {
    pub fn open(w: u32, h: u32, touch_writes: Vec<Write>) -> Result<Self> {
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error()).context("create inotify instance");
        }
        let watch = unsafe { OwnedFd::from_raw_fd(fd) };
        let dir = CString::new("/dev/input")?;
        // IN_ATTRIB too: ueventd sets a node's owner and mode right after
        // creating it, and the node may only open once that is done.
        if unsafe { libc::inotify_add_watch(fd, dir.as_ptr(), libc::IN_CREATE | libc::IN_ATTRIB) } < 0 {
            return Err(io::Error::last_os_error()).context("watch /dev/input");
        }
        let wakeful = Wakeful::new()?;
        wakeful.add(fd)?;
        let mut inputs = Inputs {
            watch,
            wakeful,
            panel: None,
            buttons: Vec::new(),
            seen: HashSet::new(),
            touch_writes,
            touch_irqs: touch_interrupts(),
            raised_at: None,
            touched_at: Instant::now(),
            reloaded_at: None,
            w: w as f32,
            h: h as f32,
        };
        inputs.scan()?;
        if inputs.panel.is_none() {
            eprintln!("no touch panel yet; waiting for one to appear under /dev/input");
        }
        Ok(inputs)
    }

    /// Take in the devices under /dev/input not seen before: the first
    /// direct multitouch device as the panel, and every device with a
    /// power, volume or back key as buttons. A panel taken in is told what
    /// the device description writes to the touchscreen.
    fn scan(&mut self) -> Result<()> {
        for (path, dev) in evdev::raw_stream::enumerate() {
            if !self.seen.insert(path.clone()) {
                continue;
            }
            let direct = dev.properties().contains(PropType::DIRECT);
            let mt = dev
                .supported_absolute_axes()
                .is_some_and(|a| a.contains(AbsoluteAxisCode::ABS_MT_POSITION_X) && a.contains(AbsoluteAxisCode::ABS_MT_POSITION_Y));
            let keys = dev.supported_keys().is_some_and(|k| KEYS.iter().any(|c| k.contains(*c)));
            eprintln!("input {}: {:?} direct={direct} multitouch={mt} keys={keys}", path.display(), dev.name().unwrap_or("?"));
            if direct && mt && self.panel.is_none() {
                self.wakeful.add(dev.as_raw_fd())?;
                self.panel = Some(Panel::new(path, dev)?);
                for w in &self.touch_writes {
                    fs::write(&w.path, &w.value).with_context(|| format!("write {} to {}", w.value, w.path))?;
                    skyward_sys::kmsg(&format!("wrote {} to {} for the touchscreen", w.value, w.path))?;
                }
            } else if keys {
                set_nonblocking(dev.as_raw_fd()).context("make buttons non-blocking")?;
                self.wakeful.add(dev.as_raw_fd())?;
                self.buttons.push((path, dev));
            }
        }
        Ok(())
    }

    /// Tell the touchscreen again what the device description writes to it.
    /// A panel that has been off comes back with the driver's own settings,
    /// and one left in the wrong mode hands over events the phone's touch
    /// layer refuses, which is a touchscreen that answers nothing.
    pub fn retell(&self) {
        for w in &self.touch_writes {
            match fs::write(&w.path, &w.value) {
                Ok(()) => {
                    let _ = skyward_sys::kmsg(&format!("wrote {} to {} for the touchscreen again", w.value, w.path));
                }
                Err(e) => {
                    let _ = skyward_sys::kmsg(&format!("could not write {} to {} again: {e:#}", w.value, w.path));
                }
            }
        }
    }

    /// Watch the touchscreen for a driver that has stopped handing events over
    /// while the controller keeps raising its interrupt — the state this
    /// phone's controller is left in when its panel goes to rest — and load
    /// the driver afresh when it finds one. `lit` whether the panel is on.
    pub fn watch_touch(&mut self, lit: bool) -> Result<()> {
        const SILENT: Duration = Duration::from_secs(2);
        const AGAIN: Duration = Duration::from_secs(60);
        if !lit || self.panel.is_none() {
            self.raised_at = None;
            return Ok(());
        }
        let irqs = touch_interrupts();
        if irqs > self.touch_irqs {
            // The controller asked for attention: an event is due of the driver.
            if self.raised_at.is_none() {
                self.raised_at = Some(Instant::now());
            }
        }
        self.touch_irqs = irqs;
        let Some(raised) = self.raised_at else {
            return Ok(());
        };
        if self.touched_at > raised || raised.elapsed() < SILENT {
            // An event came after the interrupt, so the driver is well.
            if self.touched_at > raised {
                self.raised_at = None;
            }
            return Ok(());
        }
        if self.reloaded_at.is_some_and(|at| at.elapsed() < AGAIN) {
            return Ok(());
        }
        self.raised_at = None;
        // The driver is not loaded afresh: one loaded while the panel is in use
        // leaves a kernel trace behind and a touchscreen that answers nothing,
        // which is worse than saying so. What is found is written down.
        if self.reloaded_at.is_none_or(|at| at.elapsed() >= AGAIN) {
            self.reloaded_at = Some(Instant::now());
            let _ = skyward_sys::kmsg("the touchscreen asks to be heard and is not handing events over: goodix_core is left as it is")?;
        }
        Ok(())
    }

    /// Empty the watch's queue; true when /dev/input changed.
    fn changed(&self) -> Result<bool> {
        let mut buf = [0u8; 4096];
        let mut any = false;
        loop {
            let n = unsafe { libc::read(self.watch.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n >= 0 {
                any = true;
                continue;
            }
            let err = io::Error::last_os_error();
            match err.kind() {
                ErrorKind::WouldBlock => return Ok(any),
                ErrorKind::Interrupted => continue,
                _ => return Err(err).context("read /dev/input changes"),
            }
        }
    }

    /// Whether a touch panel has come up.
    pub fn has_panel(&self) -> bool {
        self.panel.is_some()
    }

    /// Readable while a device has something queued, or /dev/input changed.
    pub fn fd(&self) -> RawFd {
        self.wakeful.fd()
    }

    /// Everything the devices have queued, as touches and key presses.
    pub fn read(&mut self) -> Result<Vec<Event>> {
        if self.changed()? {
            self.scan()?;
        }
        let mut out = Vec::new();
        let heard = out.len();
        if let Some(panel) = &mut self.panel {
            match panel.read(self.w, self.h, &mut out) {
                Ok(()) => {}
                Err(e) if gone(&e) => {
                    eprintln!("input {}: removed", panel.path.display());
                    self.seen.remove(&panel.path);
                    self.panel = None;
                }
                Err(e) => return Err(e).context("read touch events"),
            }
        }
        if out.len() > heard {
            self.touched_at = Instant::now();
        }
        let mut removed = Vec::new();
        for (i, (path, b)) in self.buttons.iter_mut().enumerate() {
            match b.fetch_events() {
                Ok(events) => {
                    // The kernel's own repeats of a key held are left out;
                    // the UI times its own.
                    for e in events {
                        if let EventSummary::Key(_, code, value @ (0 | 1)) = e.destructure()
                            && let Some(k) = key_of(code)
                        {
                            out.push(Event::Key(k, value == 1));
                        }
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(e) if gone(&e) => {
                    eprintln!("input {}: removed", path.display());
                    removed.push(i);
                }
                Err(e) => return Err(e).context("read button events"),
            }
        }
        for i in removed.into_iter().rev() {
            let (path, _) = self.buttons.remove(i);
            self.seen.remove(&path);
        }
        Ok(out)
    }
}
