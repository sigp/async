// {{{ Crate docs
//! # Async drain for slog-rs
//!
//! `slog-rs` is an ecosystem of reusable components for structured, extensible,
//! composable logging for Rust.
//!
//! `slog-async` allows building `Drain`s that offload processing to another
//! thread.  Typically, serialization and IO operations are slow enough that
//! they make logging hinder the performance of the main code. Sending log
//! records to another thread is much faster (ballpark of 100ns).
//!
//! Note: Unlike other logging solutions, `slog-rs` does not have a hardcoded
//! async logging implementation. This crate is just a reasonable reference
//! implementation. It should have good performance and work well in most use
//! cases. See the documentation and implementation for more details.
//!
//! It's relatively easy to implement your own `slog-rs` async logging. Feel
//! free to use this one as a starting point.
//!
//! ## Beware of `std::process::exit`
//!
//! When using `std::process::exit` to terminate a process with an exit code,
//! it is important to notice that destructors will not be called. This matters
//! for `slog_async` as it **prevents flushing** of the async drain and
//! **discards messages** that are not yet written.
//!
//! A way around this issue is encapsulate the construction of the logger into
//! its own function that returns before `std::process::exit` is called.
//!
//! ```
//! // ...
//! fn main() {
//!     let _exit_code = run(); // logger gets flushed as `run()` returns.
//!     // std::process::exit(exit_code) // this needs to be commented or it'll
//!                                      // end the doctest
//! }
//!
//! fn run() -> i32 {
//!    // initialize the logger
//!
//!    // ... do your thing ...
//!
//!    1 // exit code to return
//! }
//! ```
// }}}

// {{{ Imports & meta
#![warn(missing_docs)]

#[macro_use]
extern crate slog;
extern crate crossbeam_channel;
extern crate take_mut;
extern crate thread_local;

use crossbeam_channel::Sender;

use slog::{BorrowedKV, Level, Record, RecordStatic, SingleKV, KV};
use slog::{Key, OwnedKVList, Serializer};

use slog::Drain;
use std::fmt;
use std::sync;
use std::{io, thread};

use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use take_mut::take;
// }}}

/// This is the key given to the logger to filter based on pid.
pub const PID_KEY: &'static str = "pid";

/// Allows the user to enable/disable logs for processes
pub struct PIDLogControl(Sender<AsyncMsg>);

impl PIDLogControl {
    fn new(sender: Sender<AsyncMsg>) -> Self {
        PIDLogControl(sender)
    }

    /// Disables emitting logs for a specific PID.
    pub fn disable(&self, pid: usize) -> Result<(), ()> {
        // blocking task
        self.0.send(AsyncMsg::DisablePID(pid)).map_err(|_| ())
    }

    /// Enables emitting logs for a specific PID.
    pub fn enable(&self, pid: usize) -> Result<(), ()> {
        // blocking task
        self.0.send(AsyncMsg::EnablePID(pid)).map_err(|_| ())
    }

    /// Sets the emitted log level
    pub fn log_level(&self, level: slog::Level) -> Result<(), ()> {
        self.0.send(AsyncMsg::LogLevel(level)).map_err(|_| ())
    }
}

// {{{ Serializer

/// Serialize a KV to find PID value.
struct PidSerializer {
    pid: Option<usize>,
}

impl Serializer for PidSerializer {
    fn emit_arguments(
        &mut self,
        key: Key,
        val: &fmt::Arguments,
    ) -> slog::Result {
        if key == PID_KEY {
            if let Ok(id) = format!("{}", val).parse::<usize>() {
                self.pid = Some(id);
            }
        }
        Ok(())
    }
}

struct ToSendSerializer {
    kv: Box<dyn KV + Send>,
}

impl ToSendSerializer {
    fn new() -> Self {
        ToSendSerializer { kv: Box::new(()) }
    }

    fn finish(self) -> Box<dyn KV + Send> {
        self.kv
    }
}

impl Serializer for ToSendSerializer {
    fn emit_bool(&mut self, key: Key, val: bool) -> slog::Result {
        take(&mut self.kv, |kv| Box::new((kv, SingleKV(key, val))));
        Ok(())
    }
    fn emit_unit(&mut self, key: Key) -> slog::Result {
        take(&mut self.kv, |kv| Box::new((kv, SingleKV(key, ()))));
        Ok(())
    }
    fn emit_none(&mut self, key: Key) -> slog::Result {
        let val: Option<()> = None;
        take(&mut self.kv, |kv| Box::new((kv, SingleKV(key, val))));
        Ok(())
    }
    fn emit_char(&mut self, key: Key, val: char) -> slog::Result {
        take(&mut self.kv, |kv| Box::new((kv, SingleKV(key, val))));
        Ok(())
    }
    fn emit_u8(&mut self, key: Key, val: u8) -> slog::Result {
        take(&mut self.kv, |kv| Box::new((kv, SingleKV(key, val))));
        Ok(())
    }
    fn emit_i8(&mut self, key: Key, val: i8) -> slog::Result {
        take(&mut self.kv, |kv| Box::new((kv, SingleKV(key, val))));
        Ok(())
    }
    fn emit_u16(&mut self, key: Key, val: u16) -> slog::Result {
        take(&mut self.kv, |kv| Box::new((kv, SingleKV(key, val))));
        Ok(())
    }
    fn emit_i16(&mut self, key: Key, val: i16) -> slog::Result {
        take(&mut self.kv, |kv| Box::new((kv, SingleKV(key, val))));
        Ok(())
    }
    fn emit_u32(&mut self, key: Key, val: u32) -> slog::Result {
        take(&mut self.kv, |kv| Box::new((kv, SingleKV(key, val))));
        Ok(())
    }
    fn emit_i32(&mut self, key: Key, val: i32) -> slog::Result {
        take(&mut self.kv, |kv| Box::new((kv, SingleKV(key, val))));
        Ok(())
    }
    fn emit_f32(&mut self, key: Key, val: f32) -> slog::Result {
        take(&mut self.kv, |kv| Box::new((kv, SingleKV(key, val))));
        Ok(())
    }
    fn emit_u64(&mut self, key: Key, val: u64) -> slog::Result {
        take(&mut self.kv, |kv| Box::new((kv, SingleKV(key, val))));
        Ok(())
    }
    fn emit_i64(&mut self, key: Key, val: i64) -> slog::Result {
        take(&mut self.kv, |kv| Box::new((kv, SingleKV(key, val))));
        Ok(())
    }
    fn emit_f64(&mut self, key: Key, val: f64) -> slog::Result {
        take(&mut self.kv, |kv| Box::new((kv, SingleKV(key, val))));
        Ok(())
    }
    fn emit_usize(&mut self, key: Key, val: usize) -> slog::Result {
        take(&mut self.kv, |kv| Box::new((kv, SingleKV(key, val))));
        Ok(())
    }
    fn emit_isize(&mut self, key: Key, val: isize) -> slog::Result {
        take(&mut self.kv, |kv| Box::new((kv, SingleKV(key, val))));
        Ok(())
    }
    fn emit_str(&mut self, key: Key, val: &str) -> slog::Result {
        let val = val.to_owned();
        take(&mut self.kv, |kv| Box::new((kv, SingleKV(key, val))));
        Ok(())
    }
    fn emit_arguments(
        &mut self,
        key: Key,
        val: &fmt::Arguments,
    ) -> slog::Result {
        let val = fmt::format(*val);
        take(&mut self.kv, |kv| Box::new((kv, SingleKV(key, val))));
        Ok(())
    }

    #[cfg(feature = "nested-values")]
    fn emit_serde(
        &mut self,
        key: Key,
        value: &slog::SerdeValue,
    ) -> slog::Result {
        let val = value.to_sendable();
        take(&mut self.kv, |kv| Box::new((kv, SingleKV(key, val))));
        Ok(())
    }
}
// }}}

// {{{ Async
// {{{ AsyncError
/// Errors reported by `Async`
#[derive(Debug)]
pub enum AsyncError {
    /// Could not send record to worker thread due to full queue
    Full,
    /// Fatal problem - mutex or channel poisoning issue
    Fatal(Box<dyn std::error::Error>),
}

impl<T> From<crossbeam_channel::TrySendError<T>> for AsyncError {
    fn from(_: crossbeam_channel::TrySendError<T>) -> AsyncError {
        AsyncError::Full
    }
}

impl<T> From<crossbeam_channel::SendError<T>> for AsyncError {
    fn from(_: crossbeam_channel::SendError<T>) -> AsyncError {
        AsyncError::Fatal(Box::new(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "The logger thread terminated",
        )))
    }
}

impl<T> From<std::sync::PoisonError<T>> for AsyncError {
    fn from(err: std::sync::PoisonError<T>) -> AsyncError {
        AsyncError::Fatal(Box::new(io::Error::new(
            io::ErrorKind::BrokenPipe,
            err.to_string(),
        )))
    }
}

/// `AsyncResult` alias
pub type AsyncResult<T> = std::result::Result<T, AsyncError>;

// }}}

// {{{ AsyncCore
/// `AsyncCore` builder
pub struct AsyncCoreBuilder<D>
where
    D: slog::Drain<Err = slog::Never, Ok = ()> + Send + 'static,
{
    chan_size: usize,
    blocking: bool,
    drain: D,
    thread_name: Option<String>,
}

impl<D> AsyncCoreBuilder<D>
where
    D: slog::Drain<Err = slog::Never, Ok = ()> + Send + 'static,
{
    fn new(drain: D) -> Self {
        AsyncCoreBuilder {
            chan_size: 128,
            blocking: false,
            drain,
            thread_name: None,
        }
    }

    /// Configure a name to be used for the background thread.
    ///
    /// The name must not contain '\0'.
    ///
    /// # Panics
    ///
    /// If a name with '\0' is passed.
    pub fn thread_name(mut self, name: String) -> Self {
        assert!(name.find('\0').is_none(), "Name with \\'0\\' in it passed");
        self.thread_name = Some(name);
        self
    }

    /// Set channel size used to send logging records to worker thread. When
    /// buffer is full `AsyncCore` will start returning `AsyncError::Full` or block, depending on
    /// the `blocking` configuration.
    pub fn chan_size(mut self, s: usize) -> Self {
        self.chan_size = s;
        self
    }

    /// Should the logging call be blocking if the channel is full?
    ///
    /// Default is false, in which case it'll return `AsyncError::Full`.
    pub fn blocking(mut self, blocking: bool) -> Self {
        self.blocking = blocking;
        self
    }

    fn spawn_thread(self) -> (thread::JoinHandle<()>, Sender<AsyncMsg>) {
        let (tx, rx) = crossbeam_channel::bounded(self.chan_size);
        let mut builder = thread::Builder::new();
        if let Some(thread_name) = self.thread_name {
            builder = builder.name(thread_name);
        }
        let drain = self.drain;
        let join = builder
            .spawn(move || {
                let mut enabled_pids = std::collections::HashSet::new();
                let mut emit_log_level = None;
                loop {
                    match rx.recv().unwrap() {
                        AsyncMsg::Record(r) => {
                            if let Some(pid) = r.pid {
                                if !enabled_pids.contains(&pid) {
                                    continue;
                                }
                            }
                            // This is a log we want to process, if its level is sufficiently high
                            if let Some(level) = emit_log_level {
                                if r.level <= level {
                                    r.log_to(&drain).unwrap();
                                }
                            }
                        }
                        AsyncMsg::EnablePID(pid) => {
                            enabled_pids.insert(pid);
                        }
                        AsyncMsg::DisablePID(pid) => {
                            enabled_pids.remove(&pid);
                        }
                        AsyncMsg::LogLevel(level) => {
                            emit_log_level = Some(level);
                        }
                        AsyncMsg::Finish => return,
                    }
                }
            })
            .unwrap();

        (join, tx)
    }

    /// Build `AsyncCore`
    pub fn build(self) -> AsyncCore {
        self.build_no_guard()
    }

    /// Build `AsyncCore`
    pub fn build_no_guard(self) -> AsyncCore {
        let blocking = self.blocking;
        let (join, tx) = self.spawn_thread();

        AsyncCore {
            ref_sender: tx,
            tl_sender: thread_local::ThreadLocal::new(),
            join: Mutex::new(Some(join)),
            blocking,
        }
    }

    /// Build `AsyncCore` with `AsyncGuard`
    ///
    /// See `AsyncGuard` for more information.
    pub fn build_with_guard(self) -> (AsyncCore, AsyncGuard) {
        let blocking = self.blocking;
        let (join, tx) = self.spawn_thread();

        (
            AsyncCore {
                ref_sender: tx.clone(),
                tl_sender: thread_local::ThreadLocal::new(),
                join: Mutex::new(None),
                blocking,
            },
            AsyncGuard {
                join: Some(join),
                tx,
            },
        )
    }
}

/// Async guard
///
/// All `Drain`s are reference-counted by every `Logger` that uses them.
/// `Async` drain runs a worker thread and sends a termination (and flushing)
/// message only when being `drop`ed. Because of that it's actually
/// quite easy to have a left-over reference to a `Async` drain, when
/// terminating: especially on `panic`s or similar unwinding event. Typically
/// it's caused be a leftover reference like `Logger` in thread-local variable,
/// global variable, or a thread that is not being joined on. It might be a
/// program bug, but logging should work reliably especially in case of bugs.
///
/// `AsyncGuard` is a remedy: it will send a flush and termination message to
/// a `Async` worker thread, and wait for it to finish on it's own `drop`. Using it
/// is a simplest way to guarantee log flushing when using `slog_async`.
pub struct AsyncGuard {
    // Should always be `Some`. `None` only
    // after `drop`
    join: Option<thread::JoinHandle<()>>,
    tx: Sender<AsyncMsg>,
}

impl Drop for AsyncGuard {
    fn drop(&mut self) {
        let _err: Result<(), Box<dyn std::error::Error>> = {
            || {
                let _ = self.tx.send(AsyncMsg::Finish);
                let join = self.join.take().unwrap();
                if join.thread().id() != thread::current().id() {
                    // See AsyncCore::drop for rationale of this branch.
                    join.join().map_err(|_| {
                        io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "Logging thread worker join error",
                        )
                    })?;
                }
                Ok(())
            }
        }();
    }
}

/// Core of `Async` drain
///
/// See `Async` for documentation.
///
/// Wrapping `AsyncCore` allows implementing custom overflow (and other errors)
/// handling strategy.
///
/// Note: On drop `AsyncCore` waits for it's worker-thread to finish (after
/// handling all previous `Record`s sent to it). If you can't tolerate the
/// delay, make sure you drop it eg. in another thread.
pub struct AsyncCore {
    ref_sender: Sender<AsyncMsg>,
    tl_sender: thread_local::ThreadLocal<Sender<AsyncMsg>>,
    join: Mutex<Option<thread::JoinHandle<()>>>,
    blocking: bool,
}

impl AsyncCore {
    /// New `AsyncCore` with default parameters
    pub fn new<D>(drain: D) -> Self
    where
        D: slog::Drain<Err = slog::Never, Ok = ()> + Send + 'static,
        D: std::panic::RefUnwindSafe,
    {
        AsyncCoreBuilder::new(drain).build()
    }

    /// Build `AsyncCore` drain with custom parameters
    pub fn custom<
        D: slog::Drain<Err = slog::Never, Ok = ()> + Send + 'static,
    >(
        drain: D,
    ) -> AsyncCoreBuilder<D> {
        AsyncCoreBuilder::new(drain)
    }
    fn get_sender(
        &self,
    ) -> Result<
        &crossbeam_channel::Sender<AsyncMsg>,
        std::sync::PoisonError<
            sync::MutexGuard<crossbeam_channel::Sender<AsyncMsg>>,
        >,
    > {
        self.tl_sender.get_or_try(|| Ok(self.ref_sender.clone()))
    }

    /// Send `AsyncRecord` to a worker thread.
    fn send(&self, r: AsyncRecord) -> AsyncResult<()> {
        let sender = self.get_sender()?;

        if self.blocking {
            sender.send(AsyncMsg::Record(r))?;
        } else {
            sender.try_send(AsyncMsg::Record(r))?;
        }

        Ok(())
    }
}

impl Drain for AsyncCore {
    type Ok = ();
    type Err = AsyncError;

    fn log(
        &self,
        record: &Record,
        logger_values: &OwnedKVList,
    ) -> AsyncResult<()> {
        self.send(AsyncRecord::from(record, logger_values))
    }
}

/// Serialized record.
pub struct AsyncRecord {
    msg: String,
    level: Level,
    location: Box<slog::RecordLocation>,
    tag: String,
    logger_values: OwnedKVList,
    kv: Box<dyn KV + Send>,
    pid: Option<usize>,
}

impl AsyncRecord {
    /// Serializes a `Record` and an `OwnedKVList`.
    pub fn from(record: &Record, logger_values: &OwnedKVList) -> Self {
        let mut ser = ToSendSerializer::new();
        record
            .kv()
            .serialize(record, &mut ser)
            .expect("`ToSendSerializer` can't fail");

        // Search for a PID
        let mut pid = PidSerializer { pid: None };
        logger_values
            .serialize(record, &mut pid)
            .expect("Cannot fail");

        AsyncRecord {
            msg: fmt::format(*record.msg()),
            level: record.level(),
            location: Box::new(*record.location()),
            tag: String::from(record.tag()),
            logger_values: logger_values.clone(),
            pid: pid.pid,
            kv: ser.finish(),
        }
    }

    /// Writes the record to a `Drain`.
    pub fn log_to<D: Drain>(self, drain: &D) -> Result<D::Ok, D::Err> {
        let rs = RecordStatic {
            location: &*self.location,
            level: self.level,
            tag: &self.tag,
        };

        drain.log(
            &Record::new(
                &rs,
                &format_args!("{}", self.msg),
                BorrowedKV(&self.kv),
            ),
            &self.logger_values,
        )
    }

    /// Deconstruct this `AsyncRecord` into a record and `OwnedKVList`.
    pub fn as_record_values(&self, mut f: impl FnMut(&Record, &OwnedKVList)) {
        let rs = RecordStatic {
            location: &*self.location,
            level: self.level,
            tag: &self.tag,
        };

        f(
            &Record::new(
                &rs,
                &format_args!("{}", self.msg),
                BorrowedKV(&self.kv),
            ),
            &self.logger_values,
        )
    }
}

enum AsyncMsg {
    Record(AsyncRecord),
    // Disables a PID.
    DisablePID(usize),
    // Enables a PID.
    EnablePID(usize),
    // Sets the emitted log level
    LogLevel(slog::Level),
    // Ends the task
    Finish,
}

impl Drop for AsyncCore {
    fn drop(&mut self) {
        let _err: Result<(), Box<dyn std::error::Error>> = {
            || {
                if let Some(join) = self.join.lock()?.take() {
                    let _ = self.get_sender()?.send(AsyncMsg::Finish);
                    if join.thread().id() != thread::current().id() {
                        // A custom Drain::log implementation could dynamically
                        // swap out the logger which eventually invokes
                        // AsyncCore::drop in the worker thread.
                        // If we drop the AsyncCore inside the logger thread,
                        // this join() either panic or dead-lock.
                        // TODO: Figure out whether skipping join() instead of
                        // panicking is desirable.
                        join.join().map_err(|_| {
                            io::Error::new(
                                io::ErrorKind::BrokenPipe,
                                "Logging thread worker join error",
                            )
                        })?;
                    }
                }
                Ok(())
            }
        }();
    }
}
// }}}

/// Behavior used when the channel is full.
///
/// # Note
///
/// More variants may be added in the future, without considering it a breaking change.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum OverflowStrategy {
    /// The message gets dropped and a message with number of dropped is produced once there's
    /// space.
    ///
    /// This is the default.
    ///
    /// Note that the message with number of dropped messages takes one slot in the channel as
    /// well.
    DropAndReport,
    /// The message gets dropped silently.
    Drop,
    /// The caller is blocked until there's enough space.
    Block,
    #[doc(hidden)]
    DoNotMatchAgainstThisAndReadTheDocs,
}

/// `Async` builder
pub struct AsyncBuilder<D>
where
    D: slog::Drain<Err = slog::Never, Ok = ()> + Send + 'static,
{
    core: AsyncCoreBuilder<D>,
    // Increment a counter whenever a message is dropped due to not fitting inside the channel.
    inc_dropped: bool,
}

impl<D> AsyncBuilder<D>
where
    D: slog::Drain<Err = slog::Never, Ok = ()> + Send + 'static,
{
    fn new(drain: D) -> AsyncBuilder<D> {
        AsyncBuilder {
            core: AsyncCoreBuilder::new(drain),
            inc_dropped: true,
        }
    }

    /// Set channel size used to send logging records to worker thread. When
    /// buffer is full `AsyncCore` will start returning `AsyncError::Full`.
    pub fn chan_size(self, s: usize) -> Self {
        AsyncBuilder {
            core: self.core.chan_size(s),
            ..self
        }
    }

    /// Sets what will happen if the channel is full.
    pub fn overflow_strategy(
        self,
        overflow_strategy: OverflowStrategy,
    ) -> Self {
        let (block, inc) = match overflow_strategy {
            OverflowStrategy::Block => (true, false),
            OverflowStrategy::Drop => (false, false),
            OverflowStrategy::DropAndReport => (false, true),
            OverflowStrategy::DoNotMatchAgainstThisAndReadTheDocs => {
                panic!("Invalid variant")
            }
        };
        AsyncBuilder {
            core: self.core.blocking(block),
            inc_dropped: inc,
        }
    }

    /// Configure a name to be used for the background thread.
    ///
    /// The name must not contain '\0'.
    ///
    /// # Panics
    ///
    /// If a name with '\0' is passed.
    pub fn thread_name(self, name: String) -> Self {
        AsyncBuilder {
            core: self.core.thread_name(name),
            ..self
        }
    }

    /// Complete building `Async`
    pub fn build(self) -> Async {
        Async {
            core: self.core.build_no_guard(),
            dropped: AtomicUsize::new(0),
            inc_dropped: self.inc_dropped,
        }
    }

    /// Complete building `Async`
    pub fn build_no_guard(self) -> Async {
        Async {
            core: self.core.build_no_guard(),
            dropped: AtomicUsize::new(0),
            inc_dropped: self.inc_dropped,
        }
    }

    /// Complete building `Async` with PID channel
    pub fn build_with_channel(self) -> (Async, PIDLogControl) {
        let async_struct = Async {
            core: self.core.build_no_guard(),
            dropped: AtomicUsize::new(0),
            inc_dropped: self.inc_dropped,
        };
        let log_control =
            PIDLogControl::new(async_struct.core.ref_sender.clone());
        (async_struct, log_control)
    }

    /// Complete building `Async` with `AsyncGuard`
    ///
    /// See `AsyncGuard` for more information.
    pub fn build_with_guard(self) -> (Async, AsyncGuard) {
        let (core, guard) = self.core.build_with_guard();
        (
            Async {
                core,
                dropped: AtomicUsize::new(0),
                inc_dropped: self.inc_dropped,
            },
            guard,
        )
    }
}

/// Async drain
///
/// `Async` will send all the logging records to a wrapped drain running in
/// another thread.
///
/// `Async` never returns `AsyncError::Full`.
///
/// `Record`s are passed to the worker thread through a channel with a bounded
/// size (see `AsyncBuilder::chan_size`). On channel overflow `Async` will
/// start dropping `Record`s and log a message informing about it after
/// sending more `Record`s is possible again. The exact details of handling
/// overflow is implementation defined, might change and should not be relied
/// on, other than message won't be dropped as long as channel does not
/// overflow.
///
/// Any messages reported by `Async` will contain `slog-async` logging `Record`
/// tag to allow easy custom handling.
///
/// Note: On drop `Async` waits for it's worker-thread to finish (after handling
/// all previous `Record`s sent to it). If you can't tolerate the delay, make
/// sure you drop it eg. in another thread.
pub struct Async {
    core: AsyncCore,
    dropped: AtomicUsize,
    // Increment the dropped counter if dropped?
    inc_dropped: bool,
}

impl Async {
    /// New `AsyncCore` with default parameters
    pub fn default<
        D: slog::Drain<Err = slog::Never, Ok = ()> + Send + 'static,
    >(
        drain: D,
    ) -> Self {
        AsyncBuilder::new(drain).build()
    }

    /// Build `Async` drain with custom parameters
    ///
    /// The wrapped drain must handle all results (`Drain<Ok=(),Error=Never>`)
    /// since there's no way to return it back. See `slog::DrainExt::fuse()` and
    /// `slog::DrainExt::ignore_res()` for typical error handling strategies.
    pub fn new<D: slog::Drain<Err = slog::Never, Ok = ()> + Send + 'static>(
        drain: D,
    ) -> AsyncBuilder<D> {
        AsyncBuilder::new(drain)
    }

    fn push_dropped(&self, logger_values: &OwnedKVList) -> AsyncResult<()> {
        let dropped = self.dropped.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            match self.core.log(
                &record!(
                    slog::Level::Error,
                    "slog-async",
                    &format_args!(
                        "slog-async: logger dropped messages \
                         due to channel \
                         overflow"
                    ),
                    b!("count" => dropped)
                ),
                logger_values,
            ) {
                Ok(()) => {}
                Err(AsyncError::Full) => {
                    self.dropped.fetch_add(dropped + 1, Ordering::Relaxed);
                    return Ok(());
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

impl Drain for Async {
    type Ok = ();
    type Err = AsyncError;

    // TODO: Review `Ordering::Relaxed`
    fn log(
        &self,
        record: &Record,
        logger_values: &OwnedKVList,
    ) -> AsyncResult<()> {
        self.push_dropped(logger_values)?;

        match self.core.log(record, logger_values) {
            Ok(()) => {}
            Err(AsyncError::Full) if self.inc_dropped => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(AsyncError::Full) => {}
            Err(e) => return Err(e),
        }

        Ok(())
    }
}

impl Drop for Async {
    fn drop(&mut self) {
        let _ = self.push_dropped(&o!().into());
    }
}

// }}}

#[cfg(test)]
mod test {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn integration_test() {
        let (mock_drain, mock_drain_rx) = MockDrain::new();
        let async_drain = AsyncBuilder::new(mock_drain).build();
        let slog =
            slog::Logger::root(async_drain.fuse(), o!("field1" => "value1"));

        info!(slog, "Message 1"; "field2" => "value2");
        warn!(slog, "Message 2"; "field3" => "value3");
        assert_eq!(
            mock_drain_rx.recv().unwrap(),
            r#"INFO Message 1: [("field1", "value1"), ("field2", "value2")]"#
        );
        assert_eq!(
            mock_drain_rx.recv().unwrap(),
            r#"WARN Message 2: [("field1", "value1"), ("field3", "value3")]"#
        );
    }

    /// Test-helper drain
    #[derive(Debug)]
    struct MockDrain {
        tx: mpsc::Sender<String>,
    }

    impl MockDrain {
        fn new() -> (Self, mpsc::Receiver<String>) {
            let (tx, rx) = mpsc::channel();
            (Self { tx }, rx)
        }
    }

    impl slog::Drain for MockDrain {
        type Ok = ();
        type Err = slog::Never;

        fn log(
            &self,
            record: &Record,
            logger_kv: &OwnedKVList,
        ) -> Result<Self::Ok, Self::Err> {
            let mut serializer = MockSerializer::default();
            logger_kv.serialize(record, &mut serializer).unwrap();
            record.kv().serialize(record, &mut serializer).unwrap();
            let level = record.level().as_short_str();
            let msg = record.msg().to_string();
            let entry = format!("{} {}: {:?}", level, msg, serializer.kvs);
            self.tx.send(entry).unwrap();
            Ok(())
        }
    }

    #[derive(Default)]
    struct MockSerializer {
        kvs: Vec<(String, String)>,
    }

    impl slog::Serializer for MockSerializer {
        fn emit_arguments(
            &mut self,
            key: Key,
            val: &fmt::Arguments,
        ) -> Result<(), slog::Error> {
            self.kvs.push((key.to_string(), val.to_string()));
            Ok(())
        }
    }
}

// vim: foldmethod=marker foldmarker={{{,}}}
