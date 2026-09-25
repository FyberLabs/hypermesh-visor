use std::ffi::{c_char, c_void, CStr, CString};
use std::os::raw::c_int;

use libloading::{Library, Symbol};

use crate::desktop::{AudioRead, DesktopError, AUDIO_CHANNELS, AUDIO_RATE};

const PA_SAMPLE_S16LE: i32 = 3;
const PA_STREAM_RECORD: i32 = 2;
const CHUNK_BYTES: usize = (AUDIO_RATE as usize) * (AUDIO_CHANNELS as usize) * 2 / 10;

#[repr(C)]
struct SampleSpec {
    format: i32,
    rate: u32,
    channels: u8,
}

type NewFn = unsafe extern "C" fn(
    *const c_char,
    *const c_char,
    c_int,
    *const c_char,
    *const c_char,
    *const SampleSpec,
    *const c_void,
    *const c_void,
    *mut c_int,
) -> *mut c_void;
type ReadFn = unsafe extern "C" fn(*mut c_void, *mut c_void, usize, *mut c_int) -> c_int;
type FreeFn = unsafe extern "C" fn(*mut c_void);
type StrErrorFn = unsafe extern "C" fn(c_int) -> *const c_char;

pub struct Recorder {
    _simple: Library,
    _pulse: Library,
    handle: *mut c_void,
    read: ReadFn,
    free: FreeFn,
    strerror: StrErrorFn,
}

// The Pulse handle stays with this value and is used on one thread at a time.
unsafe impl Send for Recorder {}

impl AudioRead for Recorder {
    fn read_chunk(&mut self) -> Result<Option<Vec<u8>>, DesktopError> {
        let mut buf = vec![0u8; CHUNK_BYTES];
        let mut err = 0;
        let rc = unsafe {
            (self.read)(
                self.handle,
                buf.as_mut_ptr() as *mut c_void,
                buf.len(),
                &mut err,
            )
        };
        if rc < 0 {
            return Err(DesktopError::AudioUnavailable(self.error_text(err)));
        }
        Ok(Some(buf))
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { (self.free)(self.handle) };
            self.handle = std::ptr::null_mut();
        }
    }
}

impl Recorder {
    fn error_text(&self, err: c_int) -> String {
        let ptr = unsafe { (self.strerror)(err) };
        if ptr.is_null() {
            return format!("desktop audio unavailable ({err})");
        }
        let text = unsafe { CStr::from_ptr(ptr) }.to_string_lossy();
        format!("desktop audio unavailable: {text}")
    }
}

pub fn open() -> Result<Recorder, DesktopError> {
    let mut last = None;
    for soname in ["libpulse-simple.so.0", "libpulse-simple.so"] {
        match open_named(soname) {
            Ok(recorder) => return Ok(recorder),
            Err(err) => last = Some(err),
        }
    }
    Err(last
        .unwrap_or_else(|| DesktopError::AudioUnavailable("libpulse-simple was not found".into())))
}

pub(crate) fn open_named(soname: &str) -> Result<Recorder, DesktopError> {
    let simple = unsafe { Library::new(soname) }.map_err(|err| {
        DesktopError::AudioUnavailable(format!("libpulse-simple not loaded ({err})"))
    })?;
    let pulse = unsafe { Library::new("libpulse.so.0") }
        .or_else(|_| unsafe { Library::new("libpulse.so") })
        .map_err(|err| DesktopError::AudioUnavailable(format!("libpulse not loaded ({err})")))?;
    let new_fn = load_fn::<NewFn>(&simple, b"pa_simple_new\0")?;
    let read = load_fn::<ReadFn>(&simple, b"pa_simple_read\0")?;
    let free = load_fn::<FreeFn>(&simple, b"pa_simple_free\0")?;
    let strerror = load_fn::<StrErrorFn>(&pulse, b"pa_strerror\0")?;
    let spec = SampleSpec {
        format: PA_SAMPLE_S16LE,
        rate: AUDIO_RATE,
        channels: AUDIO_CHANNELS,
    };
    let name = CString::new("hypermesh-visor").unwrap();
    let stream = CString::new("desktop").unwrap();
    let device = CString::new("@DEFAULT_MONITOR@").unwrap();
    let mut err = 0;
    let handle = unsafe {
        new_fn(
            std::ptr::null(),
            name.as_ptr(),
            PA_STREAM_RECORD,
            device.as_ptr(),
            stream.as_ptr(),
            &spec,
            std::ptr::null(),
            std::ptr::null(),
            &mut err,
        )
    };
    if handle.is_null() {
        let text = unsafe { strerror(err) };
        let detail = if text.is_null() {
            format!("{err}")
        } else {
            unsafe { CStr::from_ptr(text) }
                .to_string_lossy()
                .into_owned()
        };
        return Err(DesktopError::AudioUnavailable(format!(
            "desktop audio unavailable: {detail}"
        )));
    }
    Ok(Recorder {
        _simple: simple,
        _pulse: pulse,
        handle,
        read,
        free,
        strerror,
    })
}

fn load_fn<T>(lib: &Library, symbol: &[u8]) -> Result<T, DesktopError>
where
    T: Copy,
{
    let sym: Symbol<T> = unsafe { lib.get(symbol) }
        .map_err(|err| DesktopError::AudioUnavailable(format!("pulse symbol missing ({err})")))?;
    Ok(*sym)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_library_is_an_error() {
        let Err(err) = open_named("libpulse-simple-does-not-exist.so") else {
            panic!("missing pulse library was loaded");
        };
        assert!(matches!(err, DesktopError::AudioUnavailable(_)));
        let text = err.to_string();
        assert!(text.contains("libpulse-simple"));
    }
}
