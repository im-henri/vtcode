//! A small neural language model for inline prediction.
//!
//! The pattern predictor only knows what the open files show. This model adds
//! general knowledge of how text continues. It is a causal transformer over
//! subword tokens (`subwords`): it reads any text as byte pairs, and writes
//! any text, including names and words it has never seen. Before its window
//! it reads two passages from outside it: a place elsewhere that shares words
//! with the text around the caret, and the text just below the caret.
//!
//! The weights are some megabytes of signed bytes built into the editor.
//! Evaluation is plain CPU arithmetic, and results are kept per position:
//! typing one more token computes one more position. Positions are relative,
//! so the window slides along with the caret and nothing is computed twice.
use super::subwords::{Cache, Subwords};

const MAGIC: &[u8] = b"VTNT6\0";
const LANGUAGES: usize = 24;
/// Positions computed together, so that each weight matrix is read once per
/// group.
const BLOCK: usize = 64;
/// A few tokens appended at once are taken for typing; more for reading on.
const TYPED_AT_ONCE: usize = 48;
/// Longest continuation written, in tokens and in bytes.
const MAX_WRITTEN: usize = 40;
const MAX_WRITTEN_BYTES: usize = 200;
/// Most line ends written into an open block.
const MAX_WRITTEN_LINES: usize = 6;

/// Settings for weighing a line against the text below it (`VT_RANK`
/// overrides them for measuring; `VT_RANK=0` turns it off):
/// choices tried, the probability below which a token is doubtful, tokens
/// of the text below scored, and whether the chosen token's probability
/// becomes its chance given the text below (otherwise it keeps its own).
/// `VT_RANK=choices,doubt,suffix,posterior`.
fn rank_knobs() -> (usize, f32, usize, bool) {
    static KNOBS: std::sync::OnceLock<(usize, f32, usize, bool)> = std::sync::OnceLock::new();
    *KNOBS.get_or_init(|| {
        let text = std::env::var("VT_RANK").unwrap_or_default();
        let mut parts = text.split(',').map(|part| part.trim().parse::<f32>().ok());
        let choices = parts.next().flatten().unwrap_or(4.) as usize;
        let doubt = parts.next().flatten().unwrap_or(0.7);
        let suffix = parts.next().flatten().unwrap_or(12.) as usize;
        let posterior = parts.next().flatten().unwrap_or(1.) > 0.;
        (choices, doubt, suffix, posterior)
    })
}

thread_local! {
    /// While recording: the latest line weighed against the text below, as
    /// typed bytes, the greedy line and the line chosen.
    pub(crate) static RANKED: std::cell::RefCell<Option<(usize, Vec<u8>, Vec<u8>)>> = const { std::cell::RefCell::new(None) };
}

/// Layers whose neurons are recorded, counted from the last.
const TRACED_NEURON_LAYERS: usize = 3;

/// Most positions remembered after they slid out of the window.
const MEMORY: usize = 4096;

/// Settings of the nearest-neighbour cache: share mixed in, sharpness, and
/// neighbours used; `VT_RECALL=mix,sharpness,nearest` overrides them for
/// measuring, and `VT_RECALL=0` turns the cache off.
fn recall_knobs() -> (f32, f32, usize) {
    static KNOBS: std::sync::OnceLock<(f32, f32, usize)> = std::sync::OnceLock::new();
    *KNOBS.get_or_init(|| {
        let text = std::env::var("VT_RECALL").unwrap_or_default();
        let mut parts = text.split(',').map(|part| part.trim().parse::<f32>().ok());
        let mix = parts.next().flatten().unwrap_or(0.2);
        let sharpness = parts.next().flatten().unwrap_or(20.);
        let nearest = parts.next().flatten().unwrap_or(16.) as usize;
        (mix, sharpness, nearest.max(1))
    })
}

/// Text from outside the window, which the model reads before it.
#[derive(Clone, Default, Debug, PartialEq)]
pub(crate) struct Passages {
    /// From another place: earlier in the file, or in another file.
    pub(crate) elsewhere: Vec<u16>,
    /// What follows the caret.
    pub(crate) below: Vec<u16>,
}

fn take<'a>(bytes: &mut &'a [u8], count: usize) -> Option<&'a [u8]> {
    if bytes.len() < count {
        return None;
    }
    let (head, tail) = bytes.split_at(count);
    *bytes = tail;
    Some(head)
}

fn sizes<const N: usize>(bytes: &mut &[u8]) -> Option<[usize; N]> {
    let mut sizes = [0usize; N];
    for size in &mut sizes {
        *size = u32::from_le_bytes(take(bytes, 4)?.try_into().ok()?) as usize;
    }
    Some(sizes)
}

fn floats(bytes: &mut &[u8], count: usize) -> Option<Vec<f32>> {
    Some(
        take(bytes, count.checked_mul(4)?)?
            .as_chunks::<4>()
            .0
            .iter()
            .map(|value| f32::from_le_bytes(*value))
            .collect(),
    )
}

/// A matrix as stored and as computed with: one signed byte per weight and
/// one scale for all of them. Keeping the bytes makes the model a quarter of
/// the size in memory, and reading them is most of the cost of one token.
struct Matrix<'a> {
    scale: f32,
    cells: &'a [i8],
}

impl Matrix<'_> {
    fn floats(&self) -> Vec<f32> {
        self.cells.iter().map(|&cell| f32::from(cell) * self.scale).collect()
    }

    /// Add row `index` to `cells`.
    fn add_row(&self, index: usize, cells: &mut [f32]) {
        let row = &self.cells[index * cells.len()..][..cells.len()];
        for (cell, &weight) in cells.iter_mut().zip(row) {
            *cell += f32::from(weight) * self.scale;
        }
    }
}

fn matrix<'a>(bytes: &mut &'a [u8], count: usize) -> Option<Matrix<'a>> {
    let scale = f32::from_le_bytes(take(bytes, 4)?.try_into().ok()?);
    let cells = take(bytes, count)?;
    // SAFETY: i8 and u8 have the same size and alignment, and every byte is
    // a valid i8. The borrowed weights keep the model file's lifetime.
    let cells = unsafe { std::slice::from_raw_parts(cells.as_ptr().cast(), cells.len()) };
    Some(Matrix { scale, cells })
}

fn dot_portable(a: &[f32], b: &[f32]) -> f32 {
    let mut lanes = [0f32; 8];
    let ((a_chunks, a_tail), (b_chunks, b_tail)) = (a.as_chunks::<8>(), b.as_chunks::<8>());
    for (x, y) in a_chunks.iter().zip(b_chunks) {
        for lane in 0..8 {
            lanes[lane] += x[lane] * y[lane];
        }
    }
    let tail: f32 = a_tail.iter().zip(b_tail).map(|(x, y)| x * y).sum();
    lanes.iter().sum::<f32>() + tail
}

/// Inner product of a row of byte weights with values.
fn dot_bytes(weights: &[i8], values: &[f32]) -> f32 {
    let mut lanes = [0f32; 8];
    let ((w_chunks, w_tail), (x_chunks, x_tail)) = (weights.as_chunks::<8>(), values.as_chunks::<8>());
    for (w, x) in w_chunks.iter().zip(x_chunks) {
        for lane in 0..8 {
            lanes[lane] += f32::from(w[lane]) * x[lane];
        }
    }
    let tail: f32 = w_tail.iter().zip(x_tail).map(|(&w, x)| f32::from(w) * x).sum();
    lanes.iter().sum::<f32>() + tail
}

/// Work above this many multiply-adds is spread over several threads.
const PARALLEL_WORK: usize = 1 << 21;

/// Threads used for long computations: the processor's, up to four.
fn thread_count() -> usize {
    static COUNT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *COUNT.get_or_init(|| {
        std::thread::available_parallelism()
            .map_or(1, |count| count.get())
            .min(4)
    })
}

/// Run on every helper thread before it computes, so that it can lower its
/// scheduling priority like the thread that started it.
static THREAD_SETUP: std::sync::OnceLock<fn() -> Box<dyn std::any::Any>> = std::sync::OnceLock::new();

/// Let the editor give helper threads the completion worker's low priority.
#[allow(dead_code)] // The replay benchmark has no scheduler to ask.
pub(crate) fn set_thread_setup(setup: fn() -> Box<dyn std::any::Any>) {
    let _ = THREAD_SETUP.set(setup);
}

/// Helper threads kept for the life of the process. Starting a thread costs
/// more than many of the products it would compute, so they wait for work.
struct Pool {
    state: std::sync::Mutex<PoolState>,
    wake: std::sync::Condvar,
    idle: std::sync::Condvar,
    /// Held by the one caller the helpers work for; any other computes alone.
    busy: std::sync::Mutex<()>,
    helpers: usize,
}

#[derive(Default)]
struct PoolState {
    job: Option<Job>,
    /// Counts the jobs offered, so a helper joins each at most once.
    round: u64,
    /// Helpers inside the current job.
    running: usize,
}

/// A task shared between the caller and the helpers: `run(index)` for every
/// index below `count`, each taken once from `next`.
#[derive(Clone, Copy)]
struct Job {
    run: *const (dyn Fn(usize) + Sync),
    next: *const std::sync::atomic::AtomicUsize,
    count: usize,
}

// SAFETY: the task is `Sync`, and `parallel` keeps it and the counter alive
// until no helper is inside the job.
unsafe impl Send for Job {}

impl Job {
    fn work(&self) {
        // SAFETY: see `Send` above.
        let (run, next) = unsafe { (&*self.run, &*self.next) };
        loop {
            let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if index >= self.count {
                break;
            }
            run(index);
        }
    }
}

fn pool() -> &'static Pool {
    static POOL: std::sync::OnceLock<&'static Pool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let pool: &'static Pool = Box::leak(Box::new(Pool {
            state: std::sync::Mutex::default(),
            wake: std::sync::Condvar::new(),
            idle: std::sync::Condvar::new(),
            busy: std::sync::Mutex::new(()),
            helpers: thread_count() - 1,
        }));
        for _ in 0..pool.helpers {
            std::thread::spawn(move || {
                let _setup = THREAD_SETUP.get().map(|setup| setup());
                let mut seen = 0;
                loop {
                    let job = {
                        let mut state = pool.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        loop {
                            if let Some(job) = state.job.filter(|_| state.round != seen) {
                                seen = state.round;
                                state.running += 1;
                                break job;
                            }
                            state = pool.wake.wait(state).unwrap_or_else(std::sync::PoisonError::into_inner);
                        }
                    };
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job.work()));
                    let mut state = pool.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.running -= 1;
                    if state.running == 0 {
                        pool.idle.notify_all();
                    }
                }
            });
        }
        pool
    })
}

/// Withdraws a job and waits for the helpers inside it, also when the
/// caller's own share of the work panics.
struct Withdraw(&'static Pool);

impl Drop for Withdraw {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.job = None;
        while state.running > 0 {
            state = self.0.idle.wait(state).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

/// Call `run` once for every index below `count`, sharing the calls with
/// the helper threads. Returns when all of them have finished.
fn parallel(count: usize, run: &(dyn Fn(usize) + Sync)) {
    let next = std::sync::atomic::AtomicUsize::new(0);
    // SAFETY: only the lifetime changes. `_withdraw` is dropped before `run`
    // and `next` are, and by then no helper is using them.
    let run_anywhere: &'static (dyn Fn(usize) + Sync) = unsafe { std::mem::transmute(run) };
    let job = Job {
        run: run_anywhere,
        next: &next,
        count,
    };
    let pool = pool();
    let turn = if count > 1 && pool.helpers > 0 { pool.busy.try_lock().ok() } else { None };
    let _withdraw = turn.as_ref().map(|_| {
        let mut state = pool.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.job = Some(job);
        state.round += 1;
        drop(state);
        pool.wake.notify_all();
        Withdraw(pool)
    });
    job.work();
}

/// `output[token * rows + row] = weights[row] . input[token]` for several
/// tokens at once. The weights are read once per group of tokens rather than
/// once per token, and a long product is divided between threads.
fn matmul(weights: &Matrix<'_>, cols: usize, input: &[f32], tokens: usize, output: &mut Vec<f32>) {
    let rows = weights.cells.len() / cols;
    output.clear();
    output.resize(rows * tokens, 0.);
    let pieces = if rows * cols * tokens >= PARALLEL_WORK && rows >= 16 {
        2 * thread_count()
    } else {
        1
    };
    let band = rows.div_ceil(pieces).next_multiple_of(2);
    let bands: Vec<(usize, usize)> = (0..rows)
        .step_by(band)
        .map(|start| (start, (start + band).min(rows)))
        .collect();
    let parts: Vec<std::sync::Mutex<Vec<f32>>> = bands.iter().map(|_| std::sync::Mutex::default()).collect();
    parallel(bands.len(), &|index| {
        let (start, end) = bands[index];
        let mut part = vec![0f32; (end - start) * tokens];
        product(weights.cells, cols, input, tokens, start, end, &mut part);
        *parts[index].lock().unwrap_or_else(std::sync::PoisonError::into_inner) = part;
    });
    for (&(start, end), part) in bands.iter().zip(parts) {
        let part = part.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner);
        for (row, values) in (start..end).zip(part.chunks_exact(tokens)) {
            for (token, &value) in values.iter().enumerate() {
                output[token * rows + row] = value * weights.scale;
            }
        }
    }
}

/// Rows `start..end` of `matmul` before scaling, stored row by row:
/// `part[(row - start) * tokens + token]`.
fn product(weights: &[i8], cols: usize, input: &[f32], tokens: usize, start: usize, end: usize, part: &mut [f32]) {
    #[cfg(target_arch = "x86_64")]
    if cols.is_multiple_of(8)
        && std::arch::is_x86_feature_detected!("avx2")
        && std::arch::is_x86_feature_detected!("fma")
    {
        // SAFETY: the required processor features were just detected, and
        // the shapes were checked by the caller.
        unsafe { product_avx2(weights, cols, input, tokens, start, end, part) };
        return;
    }
    for row in start..end {
        for token in 0..tokens {
            part[(row - start) * tokens + token] =
                dot_bytes(&weights[row * cols..][..cols], &input[token * cols..][..cols]);
        }
    }
}

/// Two weight rows against four tokens at a time: eight accumulators share
/// six loads per eight multiply-adds. Tokens left over (a single token most
/// of all, when the user types) take four rows at a time instead.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn product_avx2(
    weights: &[i8],
    cols: usize,
    input: &[f32],
    tokens: usize,
    start: usize,
    end: usize,
    part: &mut [f32],
) {
    use std::arch::x86_64::*;
    let sum = |value: __m256| {
        let mut lanes = [0f32; 8];
        // SAFETY: eight f32 values fit the array.
        unsafe { _mm256_storeu_ps(lanes.as_mut_ptr(), value) };
        lanes.iter().sum::<f32>()
    };
    // Eight byte weights as eight floats.
    // SAFETY (of its callers): `cell` points at eight readable bytes.
    let wide = |cell: *const i8| unsafe {
        _mm256_cvtepi32_ps(_mm256_cvtepi8_epi32(_mm_loadl_epi64(cell.cast())))
    };
    let pair_rows = start + (end - start) / 2 * 2;
    let quad_rows = start + (end - start) / 4 * 4;
    let full_tokens = tokens / 4 * 4;
    for row in (start..pair_rows).step_by(2) {
        let (w0, w1) = (&weights[row * cols..][..cols], &weights[(row + 1) * cols..][..cols]);
        for token in (0..full_tokens).step_by(4) {
            let x = [
                &input[token * cols..][..cols],
                &input[(token + 1) * cols..][..cols],
                &input[(token + 2) * cols..][..cols],
                &input[(token + 3) * cols..][..cols],
            ];
            // SAFETY: every slice holds `cols` values, a multiple of eight,
            // and loads are unaligned.
            unsafe {
                let mut acc = [_mm256_setzero_ps(); 8];
                for k in (0..cols).step_by(8) {
                    let a0 = wide(w0.as_ptr().add(k));
                    let a1 = wide(w1.as_ptr().add(k));
                    for (lane, x) in x.iter().enumerate() {
                        let value = _mm256_loadu_ps(x.as_ptr().add(k));
                        acc[lane] = _mm256_fmadd_ps(a0, value, acc[lane]);
                        acc[4 + lane] = _mm256_fmadd_ps(a1, value, acc[4 + lane]);
                    }
                }
                for lane in 0..4 {
                    part[(row - start) * tokens + token + lane] = sum(acc[lane]);
                    part[(row + 1 - start) * tokens + token + lane] = sum(acc[4 + lane]);
                }
            }
        }
    }
    for token in full_tokens..tokens {
        let x = &input[token * cols..][..cols];
        for row in (start..quad_rows).step_by(4) {
            let w = &weights[row * cols..][..4 * cols];
            // SAFETY: as above; `w` holds four rows of `cols` bytes.
            unsafe {
                let mut acc = [_mm256_setzero_ps(); 4];
                for k in (0..cols).step_by(8) {
                    let value = _mm256_loadu_ps(x.as_ptr().add(k));
                    for (lane, cell) in acc.iter_mut().enumerate() {
                        *cell = _mm256_fmadd_ps(wide(w.as_ptr().add(lane * cols + k)), value, *cell);
                    }
                }
                for (lane, cell) in acc.iter().enumerate() {
                    part[(row + lane - start) * tokens + token] = sum(*cell);
                }
            }
        }
    }
    // The few cells at the edges.
    for row in start..end {
        for token in 0..tokens {
            let done = if token < full_tokens { row < pair_rows } else { row < quad_rows };
            if !done {
                part[(row - start) * tokens + token] =
                    dot_bytes(&weights[row * cols..][..cols], &input[token * cols..][..cols]);
            }
        }
    }
}

/// One token's attention. For every head, the positions are weighed by how
/// well their key answers the query, and `out` gains the weighted mean of
/// their values. `keys` and `values` hold one row of `query.len()` cells per
/// position; `weights` is scratch space.
fn attend(
    query: &[f32],
    keys: &[f32],
    values: &[f32],
    heads: usize,
    scale: f32,
    out: &mut [f32],
    weights: &mut Vec<f32>,
) {
    let width = query.len();
    let head = width / heads;
    let count = keys.len() / width;
    weights.clear();
    weights.resize(heads * count, 0.);
    #[cfg(target_arch = "x86_64")]
    if head.is_multiple_of(8)
        && head <= 64
        && std::arch::is_x86_feature_detected!("avx2")
        && std::arch::is_x86_feature_detected!("fma")
    {
        // SAFETY: the required processor features were just detected, and
        // every row holds `width` cells.
        unsafe { attend_avx2(query, keys, values, heads, scale, out, weights) };
        return;
    }
    for part in 0..heads {
        let span = part * head..(part + 1) * head;
        let row = &mut weights[part * count..(part + 1) * count];
        for (weight, key) in row.iter_mut().zip(keys.chunks_exact(width)) {
            *weight = dot_portable(&query[span.clone()], &key[span.clone()]) * scale;
        }
        soften(row);
        for (weight, value) in row.iter().zip(values.chunks_exact(width)) {
            for (cell, value) in out[span.clone()].iter_mut().zip(&value[span.clone()]) {
                *cell += weight * value;
            }
        }
    }
}

/// `e^x` for eight values at once, for `x <= 0`, to about seven digits.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn exp8(x: std::arch::x86_64::__m256) -> std::arch::x86_64::__m256 {
    use std::arch::x86_64::*;
    let x = _mm256_max_ps(x, _mm256_set1_ps(-87.));
    // x = n ln 2 + r; e^x = 2^n e^r, with e^r from a short polynomial.
    let n = _mm256_round_ps::<{ _MM_FROUND_TO_NEAREST_INT | _MM_FROUND_NO_EXC }>(_mm256_mul_ps(
        x,
        _mm256_set1_ps(std::f32::consts::LOG2_E),
    ));
    let r = _mm256_fnmadd_ps(n, _mm256_set1_ps(0.693_359_4), x);
    let r = _mm256_fnmadd_ps(n, _mm256_set1_ps(-2.121_944_4e-4), r);
    let mut y = _mm256_set1_ps(1.987_569_1e-4);
    for term in [1.398_199_9e-3, 8.333_452e-3, 4.166_579_6e-2, 1.666_666_5e-1, 0.5] {
        y = _mm256_fmadd_ps(y, r, _mm256_set1_ps(term));
    }
    let y = _mm256_add_ps(_mm256_fmadd_ps(y, _mm256_mul_ps(r, r), r), _mm256_set1_ps(1.));
    let power = _mm256_slli_epi32::<23>(_mm256_add_epi32(_mm256_cvtps_epi32(n), _mm256_set1_epi32(127)));
    _mm256_mul_ps(y, _mm256_castsi256_ps(power))
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn attend_avx2(
    query: &[f32],
    keys: &[f32],
    values: &[f32],
    heads: usize,
    scale: f32,
    out: &mut [f32],
    weights: &mut [f32],
) {
    use std::arch::x86_64::*;
    let width = query.len();
    let head = width / heads;
    let lanes = head / 8;
    let count = keys.len() / width;
    let total = |value: __m256| {
        let mut cells = [0f32; 8];
        // SAFETY: eight f32 values fit the array.
        unsafe { _mm256_storeu_ps(cells.as_mut_ptr(), value) };
        cells.iter().sum::<f32>()
    };
    // SAFETY: `keys` and `values` hold `count` rows of `width` cells,
    // `weights` holds `heads * count` cells, `out` holds `width`; loads and
    // stores are unaligned.
    unsafe {
        // How well each position's key answers the query, head by head.
        for position in 0..count {
            for part in 0..heads {
                let asked = query.as_ptr().add(part * head);
                let key = keys.as_ptr().add(position * width + part * head);
                let mut sum = _mm256_setzero_ps();
                for lane in 0..lanes {
                    sum = _mm256_fmadd_ps(_mm256_loadu_ps(asked.add(lane * 8)), _mm256_loadu_ps(key.add(lane * 8)), sum);
                }
                *weights.get_unchecked_mut(part * count + position) = total(sum) * scale;
            }
        }
        for part in 0..heads {
            let row = &mut weights[part * count..(part + 1) * count];
            let peak = row.iter().copied().fold(f32::MIN, f32::max);
            let whole = count / 8 * 8;
            let mut sums = _mm256_setzero_ps();
            for at in (0..whole).step_by(8) {
                let cell = row.as_mut_ptr().add(at);
                let value = exp8(_mm256_sub_ps(_mm256_loadu_ps(cell), _mm256_set1_ps(peak)));
                _mm256_storeu_ps(cell, value);
                sums = _mm256_add_ps(sums, value);
            }
            let mut sum = total(sums);
            for weight in &mut row[whole..] {
                *weight = (*weight - peak).exp();
                sum += *weight;
            }
            // The weighted sum of the values, divided by the weights' total.
            let mut mixed = [_mm256_setzero_ps(); 8];
            for (position, &weight) in row.iter().enumerate() {
                let weight = _mm256_set1_ps(weight);
                let value = values.as_ptr().add(position * width + part * head);
                for (lane, cell) in mixed.iter_mut().enumerate().take(lanes) {
                    *cell = _mm256_fmadd_ps(weight, _mm256_loadu_ps(value.add(lane * 8)), *cell);
                }
            }
            let inverse = _mm256_set1_ps(1. / sum);
            for (lane, cell) in mixed.iter().enumerate().take(lanes) {
                let target = out.as_mut_ptr().add(part * head + lane * 8);
                _mm256_storeu_ps(target, _mm256_fmadd_ps(*cell, inverse, _mm256_loadu_ps(target)));
            }
        }
    }
}

fn normalize(input: &[f32], gain: &[f32], bias: &[f32], output: &mut Vec<f32>) {
    let mean = input.iter().sum::<f32>() / input.len() as f32;
    let variance = input.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / input.len() as f32;
    let inverse = 1. / (variance + 1e-5).sqrt();
    output.clear();
    output.extend(
        input
            .iter()
            .zip(gain.iter().zip(bias))
            .map(|(x, (gain, bias))| (x - mean) * inverse * gain + bias),
    );
}

/// Turn scores into probabilities.
fn soften(scores: &mut [f32]) {
    let peak = scores.iter().copied().fold(f32::MIN, f32::max);
    let mut total = 0.;
    for value in scores.iter_mut() {
        *value = (*value - peak).exp();
        total += *value;
    }
    for value in scores.iter_mut() {
        *value /= total;
    }
}

struct Block<'a> {
    gain1: Vec<f32>,
    bias1: Vec<f32>,
    query: Matrix<'a>,
    key: Matrix<'a>,
    value: Matrix<'a>,
    project: Matrix<'a>,
    gain2: Vec<f32>,
    bias2: Vec<f32>,
    expand: Matrix<'a>,
    expand_bias: Vec<f32>,
    reduce: Matrix<'a>,
    reduce_bias: Vec<f32>,
}

pub(crate) struct Network<'a> {
    subwords: Subwords,
    width: usize,
    heads: usize,
    /// Most window positions a token sees, itself included.
    length: usize,
    /// Description of each token, as input and as output.
    token: Matrix<'a>,
    language: Vec<f32>,
    /// What a position belongs to: the window, elsewhere, or below.
    segment: Vec<f32>,
    /// How a window position came to stand above the text below.
    origin: Vec<f32>,
    /// Most tokens read from elsewhere and from below the caret.
    elsewhere: usize,
    below: usize,
    /// Rotary rate of each pair of cells in a head.
    rates: Vec<f32>,
    blocks: Vec<Block<'a>>,
    final_gain: Vec<f32>,
    final_bias: Vec<f32>,
    output_bias: Vec<f32>,
}

/// Turn each pair of cells of every head by an angle given as cosine and
/// sine, one per pair of a head.
fn rotate(cells: &mut [f32], turns: &[(f32, f32)]) {
    for part in cells.chunks_exact_mut(2 * turns.len()) {
        for (pair, &(cos, sin)) in part.as_chunks_mut::<2>().0.iter_mut().zip(turns) {
            let (a, b) = (pair[0], pair[1]);
            pair[0] = a * cos - b * sin;
            pair[1] = a * sin + b * cos;
        }
    }
}

/// Per-position results kept between requests. Extending the context by one
/// token costs one position, not the whole window, and the window slides:
/// positions that fall out of view are dropped, the rest stay as computed.
/// The passages come first and stay.
#[derive(Default)]
pub(crate) struct Session {
    /// The passages' tokens, then the window's.
    tokens: Vec<u16>,
    /// For each of `tokens`: 0 when no text below is known, 1 for a token
    /// that stood there when reading began, 2 for one typed since.
    origins: Vec<u8>,
    passages: Passages,
    /// Whether the window has been read through once since it was started.
    settled: bool,
    keys: Vec<Vec<f32>>,
    values: Vec<Vec<f32>>,
    finals: Vec<f32>,
    language: u8,
    /// Index in the document of the window's first token.
    base: usize,
    /// Window positions dropped from the front so far. Every position is
    /// turned by `dropped` more than its place in `tokens` says.
    dropped: usize,
    /// Probability of every token after the last position, while it stands.
    scored: Option<Vec<f32>>,
    /// Spellings of pieces met before.
    cache: Cache,
    /// Final states of window positions that slid out of view, each with
    /// the token that followed it: what the model remembers of the text
    /// above its window.
    memory: Vec<f32>,
    memory_next: Vec<u16>,
    /// While recording: for the last position computed, every layer's
    /// output, then the neurons of the last layers.
    trace: Vec<f32>,
    neurons: Vec<f32>,
    /// Positions held when `trace` was recorded.
    trace_at: usize,
    #[cfg(test)]
    pub(crate) computed: usize,
}

impl Session {
    /// Positions held by the passages.
    fn pinned(&self) -> usize {
        (self.passages.elsewhere.len() + self.passages.below.len()).min(self.tokens.len())
    }

    /// Window positions held.
    fn held(&self) -> usize {
        self.tokens.len() - self.pinned()
    }

    fn neurons_trace(&mut self, layer: usize, cells: &[f32]) {
        if layer + TRACED_NEURON_LAYERS == self.keys.len() {
            self.neurons.clear();
        }
        self.neurons.extend_from_slice(cells);
    }

    /// Positions held in all; `Network::cut` returns to this state later.
    pub(crate) fn mark(&self) -> usize {
        self.tokens.len()
    }
}

/// What the model wrote after the session's text.
#[derive(Clone, Default, Debug)]
pub(crate) struct Written {
    /// The typed start of the unfinished piece as the model spells it,
    /// followed by the text it wrote after it.
    pub(crate) bytes: Vec<u8>,
    /// For each token written: where its bytes end, and its probability.
    pub(crate) steps: Vec<(usize, f32)>,
    /// For each token written: the token, and how sure the model was of it.
    pub(crate) tokens: Vec<u16>,
    pub(crate) evidence: Vec<Evidence>,
    /// While recording (`VT_RECORD`), the final state before each token
    /// written: what the model knew when it chose it.
    pub(crate) states: Vec<f32>,
    /// Likewise each layer's output, and the last layers' neurons.
    pub(crate) traces: Vec<f32>,
    pub(crate) neurons: Vec<f32>,
    /// Time spent weighing lines against the text below.
    pub(crate) ranking: std::time::Duration,
}

/// How sure the model was of one token it wrote.
#[derive(Clone, Copy, Default, Debug)]
#[allow(dead_code)] // All but the probability are read by the replay benchmark's recorder.
pub(crate) struct Evidence {
    /// Its probability among the tokens that agree with what was typed.
    pub(crate) probability: f32,
    /// The runner-up's probability, likewise.
    pub(crate) second: f32,
    /// Entropy of the choice, in nats.
    pub(crate) entropy: f32,
    /// Probability of all tokens agreeing with the typed text: how well the
    /// model expected what was typed.
    pub(crate) agreeing: f32,
    /// Probability of everything written through this token.
    pub(crate) chance: f32,
    /// Typed bytes still to be spelled out when it was chosen.
    pub(crate) typed: u8,
}

/// Whether written tokens keep the model's states, for collecting training
/// data for the verifier.
pub(crate) fn recording() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VT_RECORD").is_some())
}

/// Whether `spelling` agrees with the typed bytes still to be written: one
/// is the start of the other. Typing without capitals matches any.
fn agrees(spelling: &[u8], typed: &[u8], loose: bool) -> bool {
    let shared = spelling.len().min(typed.len());
    if loose {
        spelling[..shared].eq_ignore_ascii_case(&typed[..shared])
    } else {
        spelling[..shared] == typed[..shared]
    }
}

impl<'weights> Network<'weights> {
    pub(crate) fn decode(mut bytes: &'weights [u8]) -> Option<Self> {
        let bytes = &mut bytes;
        if take(bytes, MAGIC.len())? != MAGIC {
            return None;
        }
        let subwords = Subwords::decode_merges(bytes)?;
        let [width, layers, heads, length, elsewhere, below] = sizes(bytes)?;
        if width == 0
            || width > 768
            || layers == 0
            || layers > 16
            || heads == 0
            || width % heads != 0
            || (width / heads) % 2 != 0
            || !(64..=4096).contains(&length)
            || elsewhere > 1024
            || below > 1024
        {
            return None;
        }
        let vocabulary = subwords.len();
        let token = matrix(bytes, vocabulary * width)?;
        let language = matrix(bytes, LANGUAGES * width)?.floats();
        let segment = matrix(bytes, 3 * width)?.floats();
        let origin = matrix(bytes, 3 * width)?.floats();
        let pairs = width / heads / 2;
        let rates = (0..pairs)
            .map(|pair| 10000f32.powf(-(pair as f32) / pairs as f32))
            .collect();
        let mut blocks = Vec::with_capacity(layers);
        for _ in 0..layers {
            blocks.push(Block {
                gain1: floats(bytes, width)?,
                bias1: floats(bytes, width)?,
                query: matrix(bytes, width * width)?,
                key: matrix(bytes, width * width)?,
                value: matrix(bytes, width * width)?,
                project: matrix(bytes, width * width)?,
                gain2: floats(bytes, width)?,
                bias2: floats(bytes, width)?,
                expand: matrix(bytes, 4 * width * width)?,
                expand_bias: floats(bytes, 4 * width)?,
                reduce: matrix(bytes, 4 * width * width)?,
                reduce_bias: floats(bytes, width)?,
            });
        }
        let network = Self {
            final_gain: floats(bytes, width)?,
            final_bias: floats(bytes, width)?,
            output_bias: floats(bytes, vocabulary)?,
            subwords,
            width,
            heads,
            length,
            token,
            language,
            segment,
            origin,
            elsewhere,
            below,
            rates,
            blocks,
        };
        bytes.is_empty().then_some(network)
    }

    pub(crate) fn subwords(&self) -> &Subwords {
        &self.subwords
    }

    /// For studying the model: the token matrix, the final normalization's
    /// gain and bias, and the output bias, as numbers.
    #[allow(dead_code)] // Used by the replay benchmark.
    pub(crate) fn output_side(&self) -> (Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>) {
        (self.token.floats(), self.final_gain.clone(), self.final_bias.clone(), self.output_bias.clone())
    }

    /// Weights of block `layer`'s neurons, as numbers: expanding (one row of
    /// `width` per neuron), its bias, and reducing (one row of neurons per
    /// output cell).
    #[allow(dead_code)] // Used by the replay benchmark.
    pub(crate) fn neuron_weights(&self, layer: usize) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
        let block = &self.blocks[layer];
        (block.expand.floats(), block.expand_bias.clone(), block.reduce.floats())
    }

    #[allow(dead_code)] // Used by the replay benchmark.
    pub(crate) fn layers(&self) -> usize {
        self.blocks.len()
    }

    /// Window positions dropped at once when the window is full.
    fn slack(&self) -> usize {
        (self.length / 4).max(8)
    }

    /// Most tokens of a passage from elsewhere, and of the text below.
    pub(crate) fn passage_sizes(&self) -> (usize, usize) {
        (self.elsewhere, self.below)
    }

    /// Most window positions the last token sees, itself included.
    #[allow(dead_code)] // Used by tests and by the replay benchmark.
    pub(crate) fn window(&self) -> usize {
        self.length
    }

    /// Whether a request with `count` tokens before the caret continues the
    /// session's window rather than starting a new one.
    pub(crate) fn continues(&self, session: &Session, count: usize) -> bool {
        let held = session.held();
        held > 0 && session.base < count && count - session.base <= held.min(self.length) + self.slack()
    }

    /// First document token to read when `count` precede the caret: where
    /// the session's kept positions start, if the caret is among them or a
    /// little past their end, and otherwise a fresh window ending there.
    pub(crate) fn start(&self, session: &Session, count: usize) -> usize {
        if self.continues(session, count) {
            session.base
        } else {
            self.fresh_start(count)
        }
    }

    /// Where a new window starts when `count` tokens precede the caret. It
    /// holds half the window: a first reading stays quick, and the window
    /// grows to its full length as the user goes on typing.
    pub(crate) fn fresh_start(&self, count: usize) -> usize {
        count.saturating_sub(self.length / 2)
    }

    /// Read `passages`, then `tokens`, which start at token `base` of the
    /// document, reusing what the session holds of them. False when
    /// cancelled before all was read; what was read is kept.
    pub(crate) fn read(
        &self,
        session: &mut Session,
        passages: &Passages,
        tokens: &[u16],
        base: usize,
        language: u8,
        cancelled: &dyn Fn() -> bool,
    ) -> bool {
        if session.language != language || session.passages != *passages {
            // Every position was computed for the other language, or after
            // other passages.
            self.cut(session, 0);
            session.language = language;
            session.passages.clone_from(passages);
            session.passages.elsewhere.truncate(self.elsewhere);
            session.passages.below.truncate(self.below);
        }
        // Line the tokens up with the positions already held. The session
        // may have dropped its front since `base` was chosen.
        let tokens = if session.held() > 0 && base <= session.base && session.base - base < tokens.len() {
            &tokens[session.base - base..]
        } else {
            // A new window. The passages stay as computed unless the old
            // window's sliding has turned them.
            let keep = if session.dropped == 0 { session.pinned() } else { 0 };
            self.cut(session, keep);
            session.base = base;
            session.dropped = 0;
            session.settled = false;
            tokens
        };
        self.advance(session, tokens, None, cancelled)
    }

    /// Start a different document context while reusing the model buffers.
    pub(crate) fn reset_context(&self, session: &mut Session) {
        self.cut(session, 0);
        session.memory.clear();
        session.memory_next.clear();
        session.cache = Cache::default();
    }

    /// Forget every position from `mark` on.
    pub(crate) fn cut(&self, session: &mut Session, mark: usize) {
        if mark >= session.tokens.len() {
            return;
        }
        let width = self.width;
        session.tokens.truncate(mark);
        session.origins.truncate(mark);
        session.finals.truncate(mark * width);
        for cache in session.keys.iter_mut().chain(&mut session.values) {
            cache.truncate(mark * width);
        }
        session.scored = None;
    }

    /// Bring the session to its passages followed by exactly `tokens`,
    /// reusing what it holds of them. Stops early, keeping what was
    /// computed, when `cancelled` turns true. `origins` are the marks of
    /// `tokens` when they are known; otherwise new tokens are marked here.
    fn advance(
        &self,
        session: &mut Session,
        tokens: &[u16],
        origins: Option<&[u8]>,
        cancelled: &dyn Fn() -> bool,
    ) -> bool {
        if tokens.is_empty() {
            return false;
        }
        if session.keys.len() != self.blocks.len() {
            session.tokens.clear();
            session.origins.clear();
            session.finals.clear();
            session.keys.clear();
            session.values.clear();
            session.keys.resize_with(self.blocks.len(), Vec::new);
            session.values.resize_with(self.blocks.len(), Vec::new);
        }
        let passages = &session.passages;
        let pinned = passages.elsewhere.len() + passages.below.len();
        let wanted: Vec<u16> = passages
            .elsewhere
            .iter()
            .chain(&passages.below)
            .chain(tokens)
            .copied()
            .collect();
        let common = session
            .tokens
            .iter()
            .zip(&wanted)
            .take_while(|(a, b)| a == b)
            .count();
        self.cut(session, common);
        // A few tokens after a window that was read through were typed (or
        // are being predicted); anything else is text that stood there.
        let typed = session.settled && wanted.len() - common <= TYPED_AT_ONCE;
        let marks: Vec<u8> = (common..wanted.len())
            .map(|at| match origins {
                _ if at < pinned => 0,
                Some(origins) => origins[at - pinned],
                None if session.passages.below.is_empty() => 0,
                None if typed => 2,
                None => 1,
            })
            .collect();
        for (group, marks) in wanted[common..].chunks(BLOCK).zip(marks.chunks(BLOCK)) {
            if !self.push_block(session, group, marks, cancelled) {
                return false;
            }
        }
        session.settled = true;
        // Slide: when the window is full, drop its oldest positions. The
        // passages are turned along, so that they stay as far from the
        // window's start as they were.
        let width = self.width;
        let held = session.tokens.len() - pinned;
        if held > self.length {
            let drop = held - (self.length - self.slack());
            session
                .memory
                .extend_from_slice(&session.finals[pinned * width..(pinned + drop) * width]);
            session
                .memory_next
                .extend_from_slice(&session.tokens[pinned + 1..pinned + drop + 1]);
            let excess = session.memory_next.len().saturating_sub(MEMORY);
            session.memory_next.drain(..excess);
            session.memory.drain(..excess * width);
            session.tokens.drain(pinned..pinned + drop);
            session.origins.drain(pinned..pinned + drop);
            session.finals.drain(pinned * width..(pinned + drop) * width);
            let mut turns = Vec::new();
            self.turns(drop, &mut turns);
            for cache in &mut session.keys {
                for cells in cache[..pinned * width].chunks_exact_mut(width) {
                    rotate(cells, &turns);
                }
            }
            for cache in session.keys.iter_mut().chain(&mut session.values) {
                cache.drain(pinned * width..(pinned + drop) * width);
            }
            session.base += drop;
            session.dropped += drop;
        }
        true
    }

    /// Cosine and sine of the angle each pair of cells is turned by at
    /// `position`.
    fn turns(&self, position: usize, turns: &mut Vec<(f32, f32)>) {
        turns.extend(self.rates.iter().map(|rate| {
            let (sin, cos) = (position as f64 * f64::from(*rate)).sin_cos();
            (cos as f32, sin as f32)
        }));
    }

    /// Compute positions for `new` tokens, layer by layer. Keeps what was
    /// computed before and leaves the session unchanged when `cancelled`.
    fn push_block(&self, session: &mut Session, new: &[u16], origins: &[u8], cancelled: &dyn Fn() -> bool) -> bool {
        let width = self.width;
        let count = new.len();
        let first = session.tokens.len();
        fn row(table: &[f32], width: usize, index: usize) -> &[f32] {
            &table[index * width..][..width]
        }
        // Where each part starts among the session's tokens, and the
        // position its first token is turned by.
        let elsewhere = session.passages.elsewhere.len();
        let pinned = elsewhere + session.passages.below.len();
        let reference = self.elsewhere + self.below;
        let part = |at: usize| match at {
            at if at < elsewhere => (1, 0, 0),
            at if at < pinned => (2, elsewhere, self.elsewhere),
            _ => (0, pinned, reference),
        };
        session.scored = None;
        let mut hidden = vec![0f32; count * width];
        let pairs = self.rates.len();
        let mut turns = Vec::with_capacity(count * pairs);
        for (offset, &token) in new.iter().enumerate() {
            let at = first + offset;
            let (segment, start, place) = part(at);
            self.turns(session.dropped + place + at - start, &mut turns);
            session.tokens.push(token);
            session.origins.push(origins[offset]);
            let cells = &mut hidden[offset * width..][..width];
            self.token.add_row((token as usize).min(self.subwords.len() - 1), cells);
            let tables = [
                row(&self.language, width, (session.language as usize).min(LANGUAGES - 1)),
                row(&self.segment, width, segment),
                row(&self.origin, width, (origins[offset] as usize).min(2)),
            ];
            for table in tables {
                for (cell, value) in cells.iter_mut().zip(table) {
                    *cell += value;
                }
            }
        }
        let head = width / self.heads;
        let scale = 1. / (head as f32).sqrt();
        let (mut normal, mut query, mut keys, mut values) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let (mut projected, mut expanded) = (Vec::new(), Vec::new());
        let mut mixed = vec![0f32; count * width];
        let mut line = Vec::new();
        for (layer, block) in self.blocks.iter().enumerate() {
            if cancelled() {
                session.tokens.truncate(first);
                session.origins.truncate(first);
                for cache in session.keys.iter_mut().chain(&mut session.values) {
                    cache.truncate(first * width);
                }
                return false;
            }
            normal.clear();
            for cells in hidden.chunks_exact(width) {
                normalize(cells, &block.gain1, &block.bias1, &mut line);
                normal.extend(&line);
            }
            matmul(&block.query, width, &normal, count, &mut query);
            matmul(&block.key, width, &normal, count, &mut keys);
            matmul(&block.value, width, &normal, count, &mut values);
            for (offset, turns) in turns.chunks_exact(pairs).enumerate() {
                rotate(&mut query[offset * width..][..width], turns);
                rotate(&mut keys[offset * width..][..width], turns);
            }
            session.keys[layer].extend(&keys);
            session.values[layer].extend(&values);
            let (all_keys, all_values) = (&session.keys[layer], &session.values[layer]);
            mixed.fill(0.);
            // Each new token looks back over every position held. The
            // tokens are independent, so a long block is shared out.
            let attend_tokens = |offsets: std::ops::Range<usize>, out: &mut [f32]| {
                let mut weights = Vec::new();
                for (offset, out) in offsets.zip(out.chunks_exact_mut(width)) {
                    let visible = first + offset + 1;
                    attend(
                        &query[offset * width..][..width],
                        &all_keys[..visible * width],
                        &all_values[..visible * width],
                        self.heads,
                        scale,
                        out,
                        &mut weights,
                    );
                }
            };
            let pieces = if count >= 8 && 2 * count * (first + count) * width >= PARALLEL_WORK {
                2 * thread_count()
            } else {
                1
            };
            let share = count.div_ceil(pieces);
            let parts: Vec<std::sync::Mutex<&mut [f32]>> =
                mixed.chunks_mut(share * width).map(std::sync::Mutex::new).collect();
            parallel(parts.len(), &|index| {
                let mut part = parts[index].lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                let tokens = part.len() / width;
                attend_tokens(index * share..index * share + tokens, &mut part);
            });
            drop(parts);
            matmul(&block.project, width, &mixed, count, &mut projected);
            for (cell, value) in hidden.iter_mut().zip(&projected) {
                *cell += value;
            }
            normal.clear();
            for cells in hidden.chunks_exact(width) {
                normalize(cells, &block.gain2, &block.bias2, &mut line);
                normal.extend(&line);
            }
            matmul(&block.expand, width, &normal, count, &mut expanded);
            for cells in expanded.chunks_exact_mut(4 * width) {
                for (cell, bias) in cells.iter_mut().zip(&block.expand_bias) {
                    *cell = (*cell + bias).max(0.);
                }
            }
            matmul(&block.reduce, 4 * width, &expanded, count, &mut projected);
            for (cells, extra) in hidden.chunks_exact_mut(width).zip(projected.chunks_exact(width)) {
                for (cell, (value, bias)) in cells.iter_mut().zip(extra.iter().zip(&block.reduce_bias)) {
                    *cell += value + bias;
                }
            }
            if recording() {
                // For the last position: each layer's output, and the
                // neurons of the last layers, for studying the model.
                if layer == 0 {
                    session.trace.clear();
                }
                session.trace.extend_from_slice(&hidden[(count - 1) * width..]);
                session.trace_at = first + count;
                if layer + TRACED_NEURON_LAYERS >= self.blocks.len() {
                    session.neurons_trace(layer, &expanded[(count - 1) * 4 * width..]);
                }
            }
        }
        for cells in hidden.chunks_exact(width) {
            normalize(cells, &self.final_gain, &self.final_bias, &mut line);
            session.finals.extend(&line);
        }
        #[cfg(test)]
        {
            session.computed += count;
        }
        true
    }

    /// Probability of every token after the session's last position.
    fn probabilities<'a>(&self, session: &'a mut Session) -> &'a [f32] {
        if session.scored.is_none() {
            let width = self.width;
            let last = &session.finals[session.finals.len() - width..];
            let mut scores = Vec::new();
            matmul(&self.token, width, last, 1, &mut scores);
            for (score, bias) in scores.iter_mut().zip(&self.output_bias) {
                *score += bias;
            }
            // A document does not start in the middle of a line.
            let document = self.subwords.document() as usize;
            scores[document] = f32::NEG_INFINITY;
            soften(&mut scores);
            self.recall(session, session.tokens.len(), &mut scores);
            session.scored = Some(scores);
        }
        session.scored.as_deref().unwrap_or(&[])
    }

    /// Append one token being written.
    fn push(&self, session: &mut Session, token: u16, cancelled: &dyn Fn() -> bool) -> bool {
        let origin = if session.passages.below.is_empty() { 0 } else { 2 };
        self.push_block(session, &[token], &[origin], cancelled)
    }

    /// Write on after the session's text, always taking the likeliest
    /// token, to the end of the line. `typed` is the unfinished piece the
    /// text ends in: it is spelled out first, and a token's probability
    /// there is its share among the tokens that agree with what was typed.
    /// Writing stops early when `stop` turns true; the session keeps what
    /// was written, so that reading the same text later costs nothing.
    /// With `on`, the typed piece is taken to be unfinished: the token that
    /// completes it must go on past it.
    #[allow(dead_code)] // Used by tests.
    pub(crate) fn write(&self, session: &mut Session, typed: &[u8], on: bool, stop: &dyn Fn() -> bool) -> Written {
        self.write_block(session, typed, on, 0, stop)
    }

    /// Like `write`, but when `open` brackets are left open before the
    /// text, writing goes on past line ends, with the model's own
    /// indentation, until the brackets are closed and that line ends: the
    /// rest of a block. It stops early at a line end that it is unsure of.
    pub(crate) fn write_block(
        &self,
        session: &mut Session,
        typed: &[u8],
        on: bool,
        open: i32,
        stop: &dyn Fn() -> bool,
    ) -> Written {
        self.write_from(session, typed, on, open, None, stop)
    }

    /// Write like `write_block`, then weigh the line against the text that
    /// follows it. Where the line's first tokens hold a doubtful choice, the
    /// few likeliest choices there are each written out to the end of their
    /// line, and each line is scored by how likely the model finds it and
    /// then the text `below` it (the lines after the caret line, as tokens
    /// starting with the line end). By Bayes' rule this is how likely each
    /// choice is given the text on both sides: the line that leads into
    /// the code below wins, and the chosen token's probability becomes that.
    pub(crate) fn write_ranked(
        &self,
        session: &mut Session,
        typed: &[u8],
        on: bool,
        open: i32,
        below: &[u16],
        stop: &dyn Fn() -> bool,
    ) -> Written {
        let mark = session.mark();
        let greedy = self.write_from(session, typed, on, open, None, stop);
        let started = std::time::Instant::now();
        let (choices, doubt, suffix, posterior_used) = rank_knobs();
        // Text below to judge by, more than a line end, and a single line
        // to judge: a block written on has other text after it.
        if choices < 2 || below.len() < 6 || open > 0 || stop() {
            return greedy;
        }
        // The first doubtful token among the first few.
        let Some(branch) = greedy
            .evidence
            .iter()
            .take(4)
            .position(|evidence| evidence.probability < doubt)
        else {
            return greedy;
        };
        let before = if branch == 0 { 0 } else { greedy.steps[branch - 1].0 };
        let rest = &typed[before.min(typed.len())..];
        let base = mark + branch;
        if session.tokens.len() < base {
            return greedy;
        }
        self.cut(session, base);
        let mut options = self.choices(session, rest, on, choices);
        // Choices too unlikely to win are not worth writing out.
        options.retain(|&(_, probability)| probability >= 0.03);
        if options.len() < 2 {
            self.cut(session, mark);
            self.hold(session, &greedy, stop);
            return greedy;
        }
        let suffix = &below[..below.len().min(suffix)];
        let chosen = greedy.tokens[branch];
        let mut best: Option<(f32, Written)> = None;
        let mut scored = Vec::new();
        for &(token, probability) in &options {
            if stop() {
                self.cut(session, mark);
                return greedy;
            }
            self.cut(session, base);
            let line = self.write_from(session, rest, on, 0, Some(token), stop);
            let score = self.score_line(session, base, &line, probability, suffix, stop);
            // The greedy choice must be judged too, or another would win
            // only for being the one whose line could be judged.
            let Some(score) = score else {
                if token == chosen {
                    self.cut(session, mark);
                    return greedy;
                }
                continue;
            };
            scored.push((score, probability));
            if best.as_ref().is_none_or(|(top, _)| score > *top) {
                best = Some((score, line));
            }
        }
        self.cut(session, mark);
        let Some((top, mut line)) = best.filter(|_| scored.len() >= 2) else {
            self.hold(session, &greedy, stop);
            return greedy;
        };
        // The chosen token's chance given the text on both sides. Choices
        // not tried are taken to fit the text as well as the tried ones on
        // average.
        let covered: f32 = options.iter().map(|&(_, probability)| probability).sum();
        let weight: f32 = scored.iter().map(|&(score, _)| (score - top).exp()).sum();
        // Tempered: one line per choice makes the estimate too sure, so it
        // goes halfway from the token's own probability.
        let own = line.evidence.first().map_or(0., |evidence| evidence.probability);
        let posterior = (own * (covered / weight.max(1e-9)).min(1.)).sqrt();
        if posterior_used {
            if let Some(first) = line.evidence.first_mut() {
                first.probability = posterior;
            }
            if let Some(first) = line.steps.first_mut() {
                first.1 = posterior;
            }
        }
        // The greedy line's start before the branch, then the chosen line.
        let mut merged = Written {
            bytes: greedy.bytes[..before].to_vec(),
            steps: greedy.steps[..branch].to_vec(),
            tokens: greedy.tokens[..branch].to_vec(),
            evidence: greedy.evidence[..branch].to_vec(),
            ..Written::default()
        };
        let shift = merged.bytes.len();
        merged.bytes.extend(&line.bytes);
        merged.steps.extend(line.steps.iter().map(|&(end, probability)| (end + shift, probability)));
        merged.tokens.append(&mut line.tokens);
        merged.evidence.append(&mut line.evidence);
        if recording() {
            RANKED.with(|ranked| {
                *ranked.borrow_mut() = Some((typed.len(), greedy.bytes.clone(), merged.bytes.clone()));
            });
        }
        self.hold(session, &merged, stop);
        merged.ranking = started.elapsed();
        merged
    }

    /// Leave the session holding what was written, as writing does, so that
    /// reading the same text again costs nothing.
    fn hold(&self, session: &mut Session, written: &Written, stop: &dyn Fn() -> bool) {
        if let Some((_, tokens)) = written.tokens.split_last()
            && !tokens.is_empty()
        {
            let origin = if session.passages.below.is_empty() { 0 } else { 2 };
            let origins = vec![origin; tokens.len()];
            self.push_block(session, tokens, &origins, stop);
        }
    }

    /// The `count` likeliest next tokens that agree with the typed `rest`,
    /// with their probabilities among those that do.
    fn choices(&self, session: &mut Session, rest: &[u8], on: bool, count: usize) -> Vec<(u16, f32)> {
        let loose = !rest.iter().any(u8::is_ascii_uppercase);
        let subwords = &self.subwords;
        let probabilities = self.probabilities(session);
        let mut found: Vec<(u16, f32)> = Vec::new();
        let mut total = 0f32;
        for (token, &probability) in probabilities.iter().enumerate() {
            if probability < 1e-5 {
                continue;
            }
            let spelling = subwords.bytes(token as u16);
            if spelling.is_empty() || !agrees(spelling, rest, loose) {
                continue;
            }
            let continues = |byte: &u8| byte.is_ascii_alphanumeric() || *byte == b'_';
            if on && !rest.is_empty() && spelling.len() >= rest.len() && !spelling.get(rest.len()).is_some_and(continues) {
                continue;
            }
            total += probability;
            found.push((token as u16, probability));
        }
        found.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));
        found.truncate(count);
        for choice in &mut found {
            choice.1 /= total.max(1e-9);
        }
        found
    }

    /// Log-probability of a line written from position `base`, whose first
    /// token had `first` probability, and of the text `suffix` after it.
    /// None when the line does not end within what was written, so that the
    /// text below cannot follow it.
    fn score_line(
        &self,
        session: &mut Session,
        base: usize,
        line: &Written,
        first: f32,
        suffix: &[u16],
        stop: &dyn Fn() -> bool,
    ) -> Option<f32> {
        let last = line.tokens.len().checked_sub(1)?;
        // The choice's own probability, then the evidence of the text below.
        // The rest of the line is not counted: a short line would win for
        // having fewer tokens to be unsure of.
        let mut score = first.max(1e-9).ln();
        // The line must end in its last token, a line end; the text below
        // starts with its own line end, which stands in for it.
        if suffix.is_empty() || !self.subwords.bytes(line.tokens[last]).starts_with(b"\n") || last == 0 {
            return None;
        }
        // Positions held: the line's tokens but the last.
        let held = base + last;
        if session.tokens.len() < held {
            return None;
        }
        self.cut(session, held);
        let origins = vec![1u8; suffix.len()];
        if !self.push_block(session, suffix, &origins, stop) {
            return None;
        }
        for (offset, &token) in suffix.iter().enumerate() {
            let probabilities = self.scores_at(session, held + offset);
            score += probabilities[token as usize].max(1e-9).ln();
        }
        Some(score)
    }

    fn write_from(
        &self,
        session: &mut Session,
        typed: &[u8],
        on: bool,
        open: i32,
        forced: Option<u16>,
        stop: &dyn Fn() -> bool,
    ) -> Written {
        let mut written = Written::default();
        if session.finals.is_empty() {
            return written;
        }
        if recording() && session.trace_at != session.tokens.len() && session.tokens.len() > session.pinned() {
            // Recompute the last position, so that its trace is its own.
            let at = session.tokens.len() - 1;
            let (token, origin) = (session.tokens[at], session.origins[at]);
            self.cut(session, at);
            if !self.push_block(session, &[token], &[origin], stop) {
                return written;
            }
        }
        let loose = !typed.iter().any(u8::is_ascii_uppercase);
        let mut chance = 1f32;
        // Without a block to write into, a line end ends the writing.
        let blocks = open > 0;
        let mut open = open;
        let mut lines = 0usize;
        loop {
            let rest = &typed[written.bytes.len().min(typed.len())..];
            let subwords = &self.subwords;
            let probabilities = self.probabilities(session);
            let mut total = 0f32;
            let mut best = (0u16, 0f32);
            for (token, &probability) in probabilities.iter().enumerate() {
                if probability <= best.1 && rest.is_empty() {
                    continue;
                }
                let spelling = subwords.bytes(token as u16);
                if spelling.is_empty() || !agrees(spelling, rest, loose) {
                    continue;
                }
                let continues = |byte: &u8| byte.is_ascii_alphanumeric() || *byte == b'_';
                if on && !rest.is_empty() && spelling.len() >= rest.len() && !spelling.get(rest.len()).is_some_and(continues) {
                    continue;
                }
                total += probability;
                if probability > best.1 {
                    best = (token as u16, probability);
                }
            }
            // A choice made by the caller stands for the first token.
            if let Some(token) = forced.filter(|_| written.steps.is_empty()) {
                best = (token, probabilities[token as usize]);
                if !rest.is_empty() {
                    let spelling = subwords.bytes(token);
                    if !agrees(spelling, rest, loose) {
                        break;
                    }
                }
            }
            if best.1 <= 0. {
                break;
            }
            let (token, probability) = if rest.is_empty() { best } else { (best.0, best.1 / total.max(1e-9)) };
            // How the choice stood among the tokens it could have been:
            // the runner-up's share and the spread of all of them.
            // Only needed for studying the model: it costs a pass over the
            // whole vocabulary per token.
            let norm = if rest.is_empty() { 1. } else { total.max(1e-9) };
            let (mut second, mut entropy) = (0f32, 0f32);
            for (other, &value) in probabilities.iter().enumerate().filter(|_| recording()) {
                if value < 1e-7 {
                    continue;
                }
                if !rest.is_empty() {
                    let spelling = subwords.bytes(other as u16);
                    if spelling.is_empty() || !agrees(spelling, rest, loose) {
                        continue;
                    }
                }
                let share = value / norm;
                entropy -= share * share.ln();
                if other as u16 != token && share > second {
                    second = share;
                }
            }
            written.evidence.push(Evidence {
                probability,
                second,
                entropy,
                agreeing: if rest.is_empty() { 1. } else { total },
                chance: chance * probability,
                typed: rest.len().min(255) as u8,
            });
            written.tokens.push(token);
            if recording() {
                let width = self.width;
                written.states.extend_from_slice(&session.finals[session.finals.len() - width..]);
                let fresh = session.trace_at == session.tokens.len()
                    && session.trace.len() == width * self.blocks.len()
                    && session.neurons.len() == 4 * width * TRACED_NEURON_LAYERS;
                if fresh {
                    written.traces.extend_from_slice(&session.trace);
                    written.neurons.extend_from_slice(&session.neurons);
                } else {
                    written.traces.resize(written.traces.len() + width * self.blocks.len(), f32::NAN);
                    written.neurons.resize(written.neurons.len() + 4 * width * TRACED_NEURON_LAYERS, f32::NAN);
                }
            }
            let from = written.bytes.len().max(typed.len());
            written.bytes.extend(subwords.bytes(token));
            written.steps.push((written.bytes.len(), probability));
            chance *= probability;
            let piece = written.bytes.get(from..).unwrap_or(&[]);
            // A line end with brackets still open goes on into the block,
            // while what was written so far is likely enough.
            let mut ended = false;
            for &byte in piece {
                match byte {
                    b'(' | b'[' | b'{' => open += 1,
                    b')' | b']' | b'}' => open -= 1,
                    b'\n' => {
                        lines += 1;
                        ended |= !blocks || open <= 0 || lines >= MAX_WRITTEN_LINES || chance < 0.05;
                    }
                    _ => {}
                }
            }
            if ended
                || written.steps.len() >= MAX_WRITTEN
                || written.bytes.len() >= typed.len() + MAX_WRITTEN_BYTES
                || (written.steps.len() >= 3 && chance < 0.002)
                || !self.push(session, token, stop)
            {
                break;
            }
        }
        written
    }

    /// Probability that `text` follows the session's text, given that the
    /// unfinished piece it ends in starts with `typed`. The session is first
    /// brought back to `mark`; the tokens of `text` but the last stay read.
    pub(crate) fn chance(
        &self,
        session: &mut Session,
        mark: usize,
        typed: &[u8],
        text: &[u8],
        stop: &dyn Fn() -> bool,
    ) -> Option<f32> {
        let whole = [typed, text].concat();
        let whole = std::str::from_utf8(&whole).ok()?;
        let (mut tokens, mut starts) = (Vec::new(), Vec::new());
        self.subwords.encode(whole, &mut session.cache, &mut tokens, &mut starts);
        // Positions written before that agree with these tokens are kept.
        let agreeing = session.tokens[mark.min(session.tokens.len())..]
            .iter()
            .zip(&tokens)
            .take_while(|(a, b)| a == b)
            .count();
        let loose = !typed.iter().any(u8::is_ascii_uppercase);
        let mut chance = 1f32;
        for (index, (&token, &start)) in tokens.iter().zip(&starts).enumerate() {
            // The probabilities after the position before this token.
            let row = mark + index;
            let probabilities: Vec<f32> = if index < agreeing && row < session.tokens.len() {
                self.scores_at(session, row)
            } else {
                self.cut(session, row);
                if index > 0 && session.tokens.len() < row {
                    return None;
                }
                self.probabilities(session).to_vec()
            };
            let rest = &typed[(start as usize).min(typed.len())..];
            let mut probability = probabilities[token as usize];
            if !rest.is_empty() {
                let total: f32 = probabilities
                    .iter()
                    .enumerate()
                    .filter(|&(other, _)| {
                        let spelling = self.subwords.bytes(other as u16);
                        !spelling.is_empty() && agrees(spelling, rest, loose)
                    })
                    .map(|(_, &probability)| probability)
                    .sum();
                probability /= total.max(1e-9);
            }
            chance *= probability.min(1.);
            if index + 1 < tokens.len() && index >= agreeing {
                if chance < 1e-4 {
                    return Some(chance);
                }
                if !self.push(session, token, stop) {
                    return None;
                }
            }
        }
        Some(chance)
    }

    /// Probability of every token after position `row - 1`.
    fn scores_at(&self, session: &Session, row: usize) -> Vec<f32> {
        let width = self.width;
        let last = &session.finals[(row - 1) * width..row * width];
        let mut scores = Vec::new();
        matmul(&self.token, width, last, 1, &mut scores);
        for (score, bias) in scores.iter_mut().zip(&self.output_bias) {
            *score += bias;
        }
        scores[self.subwords.document() as usize] = f32::NEG_INFINITY;
        soften(&mut scores);
        self.recall(session, row, &mut scores);
        scores
    }

    /// Mix into `probabilities`, the model's view of the token after
    /// position `row - 1`, what followed the positions it holds whose final
    /// states are most like that one's (a nearest-neighbour cache over the
    /// model's own states: the text above the caret, the passages, and what
    /// slid out of the window).
    fn recall(&self, session: &Session, row: usize, probabilities: &mut [f32]) {
        let knobs = recall_knobs();
        if knobs.0 <= 0. || row < 2 {
            return;
        }
        let (mix, sharpness, nearest) = knobs;
        let width = self.width;
        let query = &session.finals[(row - 1) * width..row * width];
        let length = dot_portable(query, query).sqrt().max(1e-6);
        let elsewhere = session.passages.elsewhere.len();
        let pinned = session.pinned();
        let part = |at: usize| usize::from(at >= elsewhere) + usize::from(at >= pinned);
        let mut found: Vec<(f32, u16)> = Vec::with_capacity(row + session.memory_next.len());
        let mut consider = |key: &[f32], next: u16| {
            let similarity = dot_portable(query, key) / (length * dot_portable(key, key).sqrt().max(1e-6));
            found.push((similarity, next));
        };
        for at in 0..row - 1 {
            if part(at) == part(at + 1) {
                consider(&session.finals[at * width..(at + 1) * width], session.tokens[at + 1]);
            }
        }
        for (key, &next) in session.memory.chunks_exact(width).zip(&session.memory_next) {
            consider(key, next);
        }
        if found.is_empty() {
            return;
        }
        let nearest = nearest.min(found.len());
        found.select_nth_unstable_by(nearest - 1, |a, b| b.0.total_cmp(&a.0));
        found.truncate(nearest);
        let best = found.iter().map(|&(similarity, _)| similarity).fold(f32::MIN, f32::max);
        let weights: Vec<f32> = found
            .iter()
            .map(|&(similarity, _)| (sharpness * (similarity - best)).exp())
            .collect();
        let total: f32 = weights.iter().sum();
        for probability in probabilities.iter_mut() {
            *probability *= 1. - mix;
        }
        for (&(_, next), weight) in found.iter().zip(&weights) {
            probabilities[next as usize] += mix * weight / total;
        }
    }

    /// Largest difference between this implementation's probabilities and
    /// the trainer's own, over the contexts in a test-vector file written by
    /// `scripts/train_vtcomplete_inline_attention.py --vectors`.
    #[allow(dead_code)] // Used by tests and by the replay benchmark.
    pub(crate) fn difference(&self, mut vectors: &[u8]) -> Option<f32> {
        let bytes = &mut vectors;
        let [count] = sizes(bytes)?;
        if count > 1024 {
            return None;
        }
        let mut worst = 0f32;
        for _ in 0..count {
            let [elsewhere, below, window, language] = sizes(bytes)?;
            if elsewhere > self.elsewhere || below > self.below || window > 16 * 1024 {
                return None;
            }
            let size = elsewhere + below + window;
            let tokens: Vec<u16> = take(bytes, size * 2)?
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .collect();
            let origins = take(bytes, size)?;
            let expected = floats(bytes, self.subwords.len())?;
            let mut session = Session {
                language: language as u8,
                passages: Passages {
                    elsewhere: tokens[..elsewhere].to_vec(),
                    below: tokens[elsewhere..elsewhere + below].to_vec(),
                },
                ..Session::default()
            };
            if !self.advance(&mut session, &tokens[elsewhere + below..], Some(&origins[elsewhere + below..]), &|| false) {
                return None;
            }
            // The trainer has no rule against a document starting anywhere.
            let width = self.width;
            let last = &session.finals[session.finals.len() - width..];
            let mut scores = Vec::new();
            matmul(&self.token, width, last, 1, &mut scores);
            for (score, bias) in scores.iter_mut().zip(&self.output_bias) {
                *score += bias;
            }
            soften(&mut scores);
            for (ours, theirs) in scores.iter().zip(&expected) {
                worst = worst.max((ours - theirs).abs());
            }
        }
        bytes.is_empty().then_some(worst)
    }
}

/// The model shipped with the editor, or None when the asset is empty.
pub(crate) fn embedded() -> Option<&'static Network<'static>> {
    static NETWORK: std::sync::OnceLock<Option<Network<'static>>> = std::sync::OnceLock::new();
    NETWORK
        .get_or_init(|| Network::decode(include_bytes!("../assets/vtcomplete_inline_model.bin")))
        .as_ref()
}

/// Tokens of no particular text, for timing.
#[allow(dead_code)]
fn filler(count: usize, step: usize) -> Vec<u16> {
    (0..count).map(|at| (300 + (at * step) % 4000) as u16).collect()
}

/// Time reading a new window with full passages, in milliseconds, for the
/// replay benchmark.
#[allow(dead_code)]
pub(crate) fn time_cold(network: &Network<'_>, tokens: usize, rounds: usize) -> Vec<f64> {
    let passages = Passages {
        elsewhere: filler(network.elsewhere, 41),
        below: filler(network.below, 43),
    };
    let tokens = filler(tokens, 37);
    (0..rounds)
        .map(|_| {
            let started = std::time::Instant::now();
            let mut session = Session::default();
            network.read(&mut session, &passages, &tokens, 0, 1, &|| false);
            network.probabilities(&mut session);
            started.elapsed().as_secs_f64() * 1e3
        })
        .collect()
}

/// Time reading one more token after a long context, in milliseconds, for
/// the replay benchmark.
#[allow(dead_code)]
pub(crate) fn time_next(network: &Network<'_>, tokens: usize, rounds: usize) -> Vec<f64> {
    let passages = Passages {
        elsewhere: filler(network.elsewhere, 41),
        below: filler(network.below, 43),
    };
    let mut tokens = filler(tokens, 37);
    let mut session = Session::default();
    network.read(&mut session, &passages, &tokens, 0, 1, &|| false);
    (0..rounds)
        .map(|round| {
            tokens.push((300 + (round * 53) % 4000) as u16);
            let started = std::time::Instant::now();
            network.read(&mut session, &passages, &tokens, 0, 1, &|| false);
            network.probabilities(&mut session);
            started.elapsed().as_secs_f64() * 1e3
        })
        .collect()
}

#[cfg(test)]
#[path = "vtcomplete_inline_model_tests.rs"]
mod tests;
