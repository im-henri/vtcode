use std::time::Duration;

#[cfg(feature = "bracketed-paste")]
#[path = "windows_paste.rs"]
mod windows_paste;

use crossterm_winapi::{Handle, InputRecord};

use crate::event::{
    sys::windows::{parse::MouseButtonsPressed, poll::WinApiPoll},
    Event,
};

#[cfg(feature = "event-stream")]
use crate::event::sys::Waker;
use crate::event::{
    source::EventSource, sys::windows::parse::handle_mouse_event, timeout::PollTimeout,
    InternalEvent,
};

#[cfg(not(feature = "bracketed-paste"))]
use crate::event::sys::windows::parse::handle_key_event;

const INPUT_BATCH: usize = 4096;
#[cfg(feature = "bracketed-paste")]
const MAX_PASTE_BATCH: usize = 1_048_576;

pub(crate) struct WindowsEventSource {
    input: Handle,
    // Win32 input transport can expand each VT mouse/key packet into many
    // UTF-16 records. Decode this allocation in place: a converted VecDeque
    // duplicates the entire batch and makes large reads expensive to keep.
    read_buffer: Vec<winapi::um::wincon::INPUT_RECORD>,
    read_position: usize,
    read_count: usize,
    poll: WinApiPoll,
    surrogate_buffer: Option<u16>,
    mouse_buttons_pressed: MouseButtonsPressed,
    #[cfg(feature = "bracketed-paste")]
    paste: windows_paste::PasteReader,
}

impl WindowsEventSource {
    pub(crate) fn new() -> std::io::Result<WindowsEventSource> {
        // A console handle is a live kernel object, not a per-event resource.
        // Share it with the wait object instead of reopening CONIN$ for every
        // poll and native read (each open involves the console host).
        let input = Handle::current_in_handle()?;
        Ok(WindowsEventSource {
            poll: WinApiPoll::new(input.clone())?,
            input,
            read_buffer: vec![winapi::um::wincon::INPUT_RECORD::default(); INPUT_BATCH],
            read_position: 0,
            read_count: 0,

            surrogate_buffer: None,
            mouse_buttons_pressed: MouseButtonsPressed::default(),
            #[cfg(feature = "bracketed-paste")]
            paste: windows_paste::PasteReader::default(),
        })
    }

    fn read_batch(&mut self) -> std::io::Result<()> {
        // Keep sparse input small. During paste, Win32 input can expand each
        // character into dozens of records: amortize console-host handoffs
        // using a larger bounded read, then release that extra storage.
        let limit = INPUT_BATCH;
        #[cfg(feature = "bracketed-paste")]
        let limit = if self.paste.receiving() {
            // On older hosts, a small read cannot keep up with Win32 input
            // expansion and their unbounded queue can grow to gigabytes.
            // Grow only when a read filled the previous batch. Cap native
            // staging at 20 MiB, independently of the incoming text's size.
            // Sparse paste/modern hosts keep the small allocation.
            if self.read_count == self.read_buffer.len() {
                self.read_buffer
                    .len()
                    .saturating_mul(2)
                    .min(MAX_PASTE_BATCH)
            } else {
                self.read_buffer.len()
            }
        } else {
            limit
        };
        let mut limit = limit;
        if self.read_buffer.len() < limit {
            if self
                .read_buffer
                .try_reserve_exact(limit - self.read_buffer.len())
                .is_ok()
            {
                self.read_buffer
                    .resize(limit, winapi::um::wincon::INPUT_RECORD::default());
            } else {
                // An optional larger batch must not abort input and leave the
                // rest of a failed paste to execute in the caller's shell.
                // Report failure once, then drain with the existing storage.
                #[cfg(feature = "bracketed-paste")]
                self.paste.allocation_failed();
                limit = self.read_buffer.len();
            }
        } else if self.read_buffer.len() > limit {
            self.read_buffer.truncate(limit);
            self.read_buffer.shrink_to_fit();
        }
        let mut count = 0;
        // SAFETY: handle is live and records/count provide writable storage.
        if unsafe {
            winapi::um::consoleapi::ReadConsoleInputW(
                *self.input,
                self.read_buffer.as_mut_ptr(),
                limit as u32,
                &mut count,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        self.read_position = 0;
        self.read_count = count as usize;
        #[cfg(feature = "bracketed-paste")]
        if count != 0 {
            self.paste.begin_batch();
        }
        Ok(())
    }

    fn release_paste_batch(&mut self) {
        // The reader may now sleep indefinitely. Release the large staging
        // allocation before waiting, rather than retaining it until a key.
        // Never truncate native records still waiting to be decoded.
        if self.read_position == self.read_count && self.read_buffer.len() > INPUT_BATCH {
            #[cfg(feature = "bracketed-paste")]
            if self.paste.receiving() {
                return;
            }
            self.read_buffer.truncate(INPUT_BATCH);
            self.read_buffer.shrink_to_fit();
            self.read_position = 0;
            self.read_count = 0;
        }
    }
}

impl EventSource for WindowsEventSource {
    fn try_read(&mut self, timeout: Option<Duration>) -> std::io::Result<Option<InternalEvent>> {
        let poll_timeout = PollTimeout::new(timeout);
        #[cfg(feature = "bracketed-paste")]
        self.paste.set_win32_input_expected(
            crate::event::WINDOWS_WIN32_INPUT_MODE.load(std::sync::atomic::Ordering::Relaxed),
        );

        loop {
            #[cfg(feature = "bracketed-paste")]
            if let Some(event) = self.paste.pop() {
                return Ok(Some(InternalEvent::Event(event)));
            }
            if self.read_position == self.read_count {
                self.release_paste_batch();
                let wait_timeout = poll_timeout.leftover();
                #[cfg(feature = "bracketed-paste")]
                let wait_timeout = self.paste.wait_timeout(wait_timeout);
                if self.poll.poll(wait_timeout)?.unwrap_or(false) {
                    // The signaled input handle already tells us records are
                    // available. Two GetNumberOfConsoleInputEvents round trips
                    // here delayed every sparse pointer sample under CPU load.
                    self.read_batch()?;
                } else {
                    #[cfg(feature = "bracketed-paste")]
                    {
                        self.paste.flush_escape(&mut self.surrogate_buffer);
                        if let Some(event) = self.paste.pop() {
                            return Ok(Some(InternalEvent::Event(event)));
                        }
                    }
                }
            }
            {
                if self.read_position < self.read_count {
                    let record = InputRecord::from(self.read_buffer[self.read_position]);
                    self.read_position += 1;
                    let event = match record {
                        InputRecord::KeyEvent(record) => {
                            #[cfg(feature = "bracketed-paste")]
                            {
                                self.paste.push(record, &mut self.surrogate_buffer);
                                self.paste.pop()
                            }
                            #[cfg(not(feature = "bracketed-paste"))]
                            handle_key_event(record, &mut self.surrogate_buffer)
                        }
                        InputRecord::MouseEvent(record) => {
                            let mouse_event =
                                handle_mouse_event(record, &self.mouse_buttons_pressed);
                            self.mouse_buttons_pressed = record.button_state.into();

                            mouse_event
                        }
                        InputRecord::WindowBufferSizeEvent(record) => {
                            // windows starts counting at 0, unix at 1, add one to replicate unix behaviour.
                            Some(Event::Resize(
                                (record.size.x as i32 + 1) as u16,
                                (record.size.y as i32 + 1) as u16,
                            ))
                        }
                        InputRecord::FocusEvent(record) => {
                            let event = if record.set_focus {
                                Event::FocusGained
                            } else {
                                Event::FocusLost
                            };
                            Some(event)
                        }
                        _ => None,
                    };

                    if let Some(event) = event {
                        return Ok(Some(InternalEvent::Event(event)));
                    }
                }
            }

            // A complete VT mouse/key packet spans several native records.
            // Zero-time polling must decode records already read into this
            // bounded batch, rather than reporting "not ready" after ESC and
            // waking the application for each following historical endpoint.
            // Stop at the batch boundary: an incomplete packet never waits
            // for more input and a continuous producer cannot extend this call.
            // Test the boundary first. Checking elapsed() for every expanded
            // Win32 record makes timed polling much slower than read(None)
            // and lets a fast paste producer build a huge console-host queue.
            if self.read_position == self.read_count && poll_timeout.elapsed() {
                return Ok(None);
            }
        }
    }

    #[cfg(feature = "event-stream")]
    fn waker(&self) -> Waker {
        self.poll.waker()
    }
}
