//! Keep worker work below input, without changing the process priority class.
//! Best effort: restricted hosts may reject a scheduling hint.

#[cfg(windows)]
thread_local! {
    static BACKGROUND: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Windows can repeatedly deschedule a normal-priority terminal UI behind CPU
/// bound applications. Favor actual input through its first presentation. The
/// dedicated input decoder may retain the guard while sleeping between reads;
/// it performs no background work or idle polling. Respect an explicitly
/// lowered/raised thread priority and restore it on every exit path.
#[must_use]
pub(crate) struct InputPriority {
    #[cfg(windows)]
    restore: Option<i32>,
    // A thread pseudo-handle always refers to the caller: this guard must not
    // move to another thread before Drop restores its creator's priority.
    _local: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl InputPriority {
    pub(crate) fn new(active: bool) -> Self {
        #[cfg(windows)]
        let restore = if active && !BACKGROUND.get() {
            unsafe {
                use windows_sys::Win32::System::Threading::{
                    GetCurrentThread, GetThreadPriority, SetThreadPriority,
                    THREAD_PRIORITY_ABOVE_NORMAL, THREAD_PRIORITY_NORMAL,
                };
                let thread = GetCurrentThread();
                let original = GetThreadPriority(thread);
                (original == THREAD_PRIORITY_NORMAL
                    && SetThreadPriority(thread, THREAD_PRIORITY_ABOVE_NORMAL) != 0)
                    .then_some(original)
            }
        } else {
            None
        };
        #[cfg(not(windows))]
        let _ = active;
        Self {
            #[cfg(windows)]
            restore,
            _local: std::marker::PhantomData,
        }
    }
}

impl Drop for InputPriority {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(priority) = self.restore {
            unsafe {
                use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority};
                let _ = SetThreadPriority(GetCurrentThread(), priority);
            }
        }
    }
}

pub(crate) fn lower_current_thread() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, GetThreadPriority, SetThreadPriority, THREAD_PRIORITY_NORMAL,
        };
        // Windows uses priority levels rather than Linux's weighted sharing.
        // Permanently lowering a worker can starve it behind another normal-
        // priority program on a busy core. Keep workers fair with other apps;
        // the scoped input boost above still favors actual UI interactions.
        BACKGROUND.set(true);
        let thread = GetCurrentThread();
        if GetThreadPriority(thread) > THREAD_PRIORITY_NORMAL {
            let _ = SetThreadPriority(thread, THREAD_PRIORITY_NORMAL);
        }
    }
    #[cfg(target_os = "linux")]
    unsafe {
        // Linux nice values belong to individual threads. Passing this TID
        // avoids changing the UI thread or raising an already-niced worker.
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        let current = libc::getpriority(libc::PRIO_PROCESS, tid);
        if current < 5 {
            let _ = libc::setpriority(libc::PRIO_PROCESS, tid, 5);
        }
    }
}

/// Optional completion CPU must yield to normal editor workers too. Scope the
/// hint so a shared executor cannot inherit it for a later foreground job.
#[must_use]
pub(crate) struct CompletionPriority {
    #[cfg(windows)]
    restore: Option<i32>,
    _local: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl CompletionPriority {
    pub(crate) fn new() -> Self {
        #[cfg(windows)]
        let restore = unsafe {
            use windows_sys::Win32::System::Threading::{
                GetCurrentThread, GetThreadPriority, SetThreadPriority,
                THREAD_PRIORITY_BELOW_NORMAL,
            };
            let thread = GetCurrentThread();
            let original = GetThreadPriority(thread);
            // GetThreadPriority returns 0x7fffffff on failure.
            (original != i32::MAX
                && original > THREAD_PRIORITY_BELOW_NORMAL
                && SetThreadPriority(thread, THREAD_PRIORITY_BELOW_NORMAL) != 0)
                .then_some(original)
        };
        Self {
            #[cfg(windows)]
            restore,
            _local: std::marker::PhantomData,
        }
    }
}

impl Drop for CompletionPriority {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(priority) = self.restore {
            unsafe {
                use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority};
                let _ = SetThreadPriority(GetCurrentThread(), priority);
            }
        }
    }
}

#[cfg(all(test, any(windows, target_os = "linux")))]
mod tests {
    use super::*;

    fn current() -> i32 {
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::System::Threading::{GetCurrentThread, GetThreadPriority};
            GetThreadPriority(GetCurrentThread())
        }
        #[cfg(target_os = "linux")]
        unsafe {
            libc::getpriority(
                libc::PRIO_PROCESS,
                libc::syscall(libc::SYS_gettid) as libc::id_t,
            )
        }
    }

    #[cfg(windows)]
    #[test]
    fn completion_priority_is_lower_than_editor_work_scoped_and_respects_existing_limits() {
        std::thread::spawn(|| {
            use windows_sys::Win32::System::Threading::{
                GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_LOWEST,
            };
            let original = current();
            {
                let _completion = CompletionPriority::new();
                assert!(current() < original);
                let _nested = CompletionPriority::new();
                let _input = InputPriority::new(true);
                assert!(current() < original);
            }
            assert_eq!(current(), original);
            assert_ne!(
                unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_LOWEST) },
                0
            );
            {
                let _completion = CompletionPriority::new();
                assert_eq!(current(), THREAD_PRIORITY_LOWEST);
            }
            assert_eq!(current(), THREAD_PRIORITY_LOWEST);
        })
        .join()
        .unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn input_priority_is_scoped_nested_and_does_not_promote_workers_or_the_process() {
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetPriorityClass};
        let parent = current();
        let process = unsafe { GetPriorityClass(GetCurrentProcess()) };
        std::thread::spawn(move || {
            let before = current();
            assert_eq!(before, 0);
            {
                let _idle = InputPriority::new(false);
                assert_eq!(current(), before);
            }
            {
                let _input = InputPriority::new(true);
                assert_eq!(current(), 1);
                {
                    let _nested = InputPriority::new(true);
                    assert_eq!(current(), 1);
                }
                {
                    let _cosmetic = InputPriority::new(false);
                    assert_eq!(current(), 1);
                }
                assert_eq!(current(), 1);
                std::thread::spawn(|| {
                    assert_eq!(current(), 0);
                    lower_current_thread();
                    let before = current();
                    let _background = InputPriority::new(true);
                    assert_eq!(
                        current(),
                        before,
                        "workers must not borrow the UI input boost"
                    );
                })
                .join()
                .unwrap();
                assert_eq!(unsafe { GetPriorityClass(GetCurrentProcess()) }, process);
            }
            assert_eq!(current(), before);
            let result = std::panic::catch_unwind(|| {
                let _input = InputPriority::new(true);
                assert_eq!(current(), 1);
                panic!("exercise unwinding");
            });
            assert!(result.is_err());
            assert_eq!(current(), before);
        })
        .join()
        .unwrap();
        assert_eq!(current(), parent);
        assert_eq!(unsafe { GetPriorityClass(GetCurrentProcess()) }, process);
    }

    #[test]
    fn worker_priority_is_local_and_idempotent() {
        let ui = current();
        std::thread::spawn(|| {
            let before = current();
            lower_current_thread();
            let after = current();
            #[cfg(windows)]
            assert_eq!(after, before.min(0));
            #[cfg(target_os = "linux")]
            assert_eq!(after, before.max(5));
            lower_current_thread();
            assert_eq!(current(), after);
        })
        .join()
        .unwrap();
        assert_eq!(current(), ui);
    }

    #[test]
    fn all_background_worker_paths_lower_only_the_worker_priority() {
        let ui = current();
        let mut workers = crate::background::BackgroundWorkers::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let send = tx.clone();
        workers
            .pool_sender()
            .send(Box::new(move || send.send(current()).unwrap()))
            .unwrap();
        let send = tx.clone();
        workers.spawn("priority-test", move || send.send(current()).unwrap());
        let send = tx.clone();
        let _cancel =
            workers.spawn_cancellable("priority-test", move |_| send.send(current()).unwrap());
        workers.spawn_untracked("priority-test", move || tx.send(current()).unwrap());
        for _ in 0..4 {
            let priority = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            #[cfg(windows)]
            assert_eq!(
                priority, 0,
                "workers need fair progress under sustained CPU contention"
            );
            #[cfg(target_os = "linux")]
            assert!(priority >= 5);
        }
        assert_eq!(current(), ui);
    }
}
