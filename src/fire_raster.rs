//! A bounded, subcell light field for cursor flames and embers.
//!
//! Particle kernels merge before tone mapping. Dense emissions consequently
//! become one flame with a hot center and soft silhouette, rather than a stack
//! of particle characters. Embers retain a separate 2×4 dot field, so several
//! tiny sparks can share one cell without merging into a blurred flame block.
//! Coordinates use terminal cells, with twice as much physical height as width.
//! Document text and compositing live in the caller.

use std::{cell::RefCell, collections::HashMap, sync::LazyLock};

use ratatui::layout::Rect;

use crate::cursor_fire::{FireParticle, FireParticleKind};

const COLUMNS: usize = 8;
const ROWS: usize = 16;
const SAMPLES: usize = COLUMNS * ROWS;
const MAX_PARTICLES: usize = 1_024;
const MAX_CELLS: usize = 4_096;
const MAX_INDEX_CELLS: usize = MAX_CELLS * 16;
const MAX_SAMPLE_WORK: usize = 1_048_576;
const KERNEL_CUTOFF: f64 = 3.2;
const KERNEL_STEPS: usize = 2_048;
const TONE_STEPS: usize = 4_096;

// Linear interpolation has <0.000001 absolute kernel error and <0.000002
// tone-map error. These small shared tables replace the transcendental work
// for every flame subcell; they do not change the 8×16 sample resolution.
static KERNEL: LazyLock<[f32; KERNEL_STEPS + 1]> = LazyLock::new(|| {
    std::array::from_fn(|index| {
        let q = index as f64 * KERNEL_CUTOFF / KERNEL_STEPS as f64;
        ((-1.6 * q).exp() * (1.0 - smoothstep((q - 1.5) / (KERNEL_CUTOFF - 1.5)))) as f32
    })
});
static TONE: LazyLock<[f32; TONE_STEPS + 1]> = LazyLock::new(|| {
    std::array::from_fn(|index| -(-(index as f32 * 16.0 / TONE_STEPS as f32)).exp_m1())
});

fn kernel_weight(q: f64, table: &[f32; KERNEL_STEPS + 1]) -> f32 {
    let position = q * (KERNEL_STEPS as f64 / KERNEL_CUTOFF);
    let index = (position as usize).min(KERNEL_STEPS - 1);
    let fraction = (position - index as f64) as f32;
    table[index] + (table[index + 1] - table[index]) * fraction
}

fn tone_map(energy: f32, table: &[f32; TONE_STEPS + 1]) -> f32 {
    if energy <= 0.0 {
        return 0.0;
    }
    let position = energy.min(16.0) * (TONE_STEPS as f32 / 16.0);
    let index = (position as usize).min(TONE_STEPS - 1);
    table[index] + (table[index + 1] - table[index]) * (position - index as f32)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FireCell {
    pub x: u16,
    pub y: u16,
    pub symbol: char,
    pub inverse: bool,
    /// The colored fraction of the selected solid block glyph. Blank cells use
    /// `opacity` directly; source-text shading uses `coverage * opacity`.
    pub coverage: f32,
    pub heat: f32,
    pub opacity: f32,
    /// Soft light behind fine sparks, independent from their crisp dot shapes.
    pub glow: f32,
}

struct Field {
    has_flame: bool,
    has_embers: bool,
    energy: [f32; SAMPLES],
    thermal_energy: [f32; SAMPLES],
    thermal_weight: [f32; SAMPLES],
    ember_energy: [f32; 8],
    ember_thermal_energy: [f32; 8],
}

impl Default for Field {
    fn default() -> Self {
        Self {
            has_flame: false,
            has_embers: false,
            energy: [0.0; SAMPLES],
            thermal_energy: [0.0; SAMPLES],
            thermal_weight: [0.0; SAMPLES],
            ember_energy: [0.0; 8],
            ember_thermal_energy: [0.0; 8],
        }
    }
}

impl Field {
    fn reset(&mut self) {
        self.has_flame = false;
        self.has_embers = false;
        self.ember_energy.fill(0.0);
        self.ember_thermal_energy.fill(0.0);
        // Leave the large flame arrays untouched until a flame contributes.
        // A coasting trail often contains only detached sparks.
    }

    fn prepare_flame(&mut self) {
        if !self.has_flame {
            self.energy.fill(0.0);
            self.thermal_energy.fill(0.0);
            self.thermal_weight.fill(0.0);
            self.has_flame = true;
        }
    }
}

/// Bounded renderer-owned storage. Reuse the particle ordering, cell index and
/// large subcell fields across frames instead of allocating a tree node and
/// three light arrays for every visible terminal cell on every frame.
#[derive(Default)]
struct RasterScratch {
    particles: Vec<usize>,
    index: HashMap<(u16, u16), usize>,
    area: Rect,
    slots: Vec<u16>,
    order: Vec<usize>,
    fields: Vec<((u16, u16), Field)>,
    used: usize,
}

impl RasterScratch {
    fn prepare(&mut self, area: Rect) {
        // Clear only touched cells using the old origin, before resizing or
        // moving the viewport. Large/synthetic rectangles stay sparse.
        for &(key, _) in &self.fields[..self.used] {
            if let Some(index) = self.slot(key) { self.slots[index] = 0; }
        }
        self.index.clear();
        self.used = 0;
        let cells = usize::from(area.width) * usize::from(area.height);
        #[cfg(test)]
        let dense = !REFERENCE_INDEX.get();
        #[cfg(not(test))]
        let dense = true;
        if dense && cells <= MAX_INDEX_CELLS {
            self.area = area;
            self.slots.resize(cells, 0);
        } else {
            self.area = Rect::default();
            self.slots.clear();
        }
    }

    // Field keys are (row, column), preserving the original output order.
    fn slot(&self, (y, x): (u16, u16)) -> Option<usize> {
        self.area.contains((x, y).into()).then(||
            usize::from(y - self.area.y) * usize::from(self.area.width) + usize::from(x - self.area.x))
    }

    fn field(&mut self, key: (u16, u16)) -> Option<&mut Field> {
        let slot = self.slot(key);
        let existing = slot.map_or_else(|| self.index.get(&key).copied(),
            |slot| usize::from(self.slots[slot]).checked_sub(1));
        let index = if let Some(index) = existing {
            index
        } else {
            if self.used == MAX_CELLS {
                return None;
            }
            let index = self.used;
            self.used += 1;
            if let Some(slot) = slot { self.slots[slot] = (index + 1) as u16; }
            else { self.index.insert(key, index); }
            if index == self.fields.len() {
                self.fields.push((key, Field::default()));
            } else {
                self.fields[index].0 = key;
                self.fields[index].1.reset();
            }
            index
        };
        Some(&mut self.fields[index].1)
    }
}

thread_local! {
    static SCRATCH: RefCell<RasterScratch> = RefCell::new(RasterScratch::default());
}

#[cfg(test)]
thread_local! {
    static REFERENCE_INDEX: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[derive(Clone, Copy)]
struct Glyph {
    symbol: char,
    inverse: bool,
    mask: u128,
    count: u32,
}

const fn rectangle(left: usize, top: usize, right: usize, bottom: usize) -> u128 {
    let mut mask = 0;
    let mut row = top;
    while row < bottom {
        let mut column = left;
        while column < right {
            mask |= 1u128 << (row * COLUMNS + column);
            column += 1;
        }
        row += 1;
    }
    mask
}

const fn glyph(symbol: char, inverse: bool, mask: u128) -> Glyph {
    Glyph {
        symbol,
        inverse,
        mask,
        count: mask.count_ones(),
    }
}

const TOP_LEFT: u128 = rectangle(0, 0, 4, 8);
const TOP_RIGHT: u128 = rectangle(4, 0, 8, 8);
const BOTTOM_LEFT: u128 = rectangle(0, 8, 4, 16);
const BOTTOM_RIGHT: u128 = rectangle(4, 8, 8, 16);
const GLYPHS: [Glyph; 39] = [
    glyph('█', false, u128::MAX),
    glyph('▁', false, rectangle(0, 14, 8, 16)),
    glyph('▂', false, rectangle(0, 12, 8, 16)),
    glyph('▃', false, rectangle(0, 10, 8, 16)),
    glyph('▄', false, rectangle(0, 8, 8, 16)),
    glyph('▅', false, rectangle(0, 6, 8, 16)),
    glyph('▆', false, rectangle(0, 4, 8, 16)),
    glyph('▇', false, rectangle(0, 2, 8, 16)),
    glyph('▔', false, rectangle(0, 0, 8, 2)),
    glyph('▆', true, rectangle(0, 0, 8, 4)),
    glyph('▅', true, rectangle(0, 0, 8, 6)),
    glyph('▀', false, rectangle(0, 0, 8, 8)),
    glyph('▃', true, rectangle(0, 0, 8, 10)),
    glyph('▂', true, rectangle(0, 0, 8, 12)),
    glyph('▁', true, rectangle(0, 0, 8, 14)),
    glyph('▏', false, rectangle(0, 0, 1, 16)),
    glyph('▎', false, rectangle(0, 0, 2, 16)),
    glyph('▍', false, rectangle(0, 0, 3, 16)),
    glyph('▌', false, rectangle(0, 0, 4, 16)),
    glyph('▋', false, rectangle(0, 0, 5, 16)),
    glyph('▊', false, rectangle(0, 0, 6, 16)),
    glyph('▉', false, rectangle(0, 0, 7, 16)),
    glyph('▕', false, rectangle(7, 0, 8, 16)),
    glyph('▊', true, rectangle(6, 0, 8, 16)),
    glyph('▋', true, rectangle(5, 0, 8, 16)),
    glyph('▐', false, rectangle(4, 0, 8, 16)),
    glyph('▍', true, rectangle(3, 0, 8, 16)),
    glyph('▎', true, rectangle(2, 0, 8, 16)),
    glyph('▏', true, rectangle(1, 0, 8, 16)),
    glyph('▘', false, TOP_LEFT),
    glyph('▝', false, TOP_RIGHT),
    glyph('▖', false, BOTTOM_LEFT),
    glyph('▗', false, BOTTOM_RIGHT),
    glyph('▚', false, TOP_LEFT | BOTTOM_RIGHT),
    glyph('▞', false, TOP_RIGHT | BOTTOM_LEFT),
    glyph('▛', false, TOP_LEFT | TOP_RIGHT | BOTTOM_LEFT),
    glyph('▜', false, TOP_LEFT | TOP_RIGHT | BOTTOM_RIGHT),
    glyph('▙', false, TOP_LEFT | BOTTOM_LEFT | BOTTOM_RIGHT),
    glyph('▟', false, TOP_RIGHT | BOTTOM_LEFT | BOTTOM_RIGHT),
];

fn valid(particle: &FireParticle) -> bool {
    particle.position.x.is_finite()
        && particle.position.y.is_finite()
        && particle.velocity.x.is_finite()
        && particle.velocity.y.is_finite()
        && particle.heat.is_finite()
        && particle.opacity.is_finite()
        && particle.radius.is_finite()
        && particle.radius > 0.0
        && particle.opacity > 0.0
}

fn smoothstep(value: f64) -> f64 {
    let value = value.clamp(0.0, 1.0);
    value * value * (3.0 - 2.0 * value)
}

/// Deposit one spark into at most four neighboring dots. Bilinear weights
/// preserve its energy across subcell and cell boundaries without a blur kernel.
fn splat_ember(area: Rect, particle: &FireParticle, fields: &mut RasterScratch) {
    let center_x = f64::from(particle.position.x);
    let center_y = f64::from(particle.position.y);
    if center_x + 0.5 < f64::from(area.x)
        || center_x - 0.5 >= f64::from(area.right())
        || center_y + 0.25 < f64::from(area.y)
        || center_y - 0.25 >= f64::from(area.bottom())
    {
        return;
    }
    let grid_x = center_x * 2.0 - 0.5;
    let grid_y = center_y * 4.0 - 0.5;
    let left = grid_x.floor() as i32;
    let top = grid_y.floor() as i32;
    let fx = (grid_x - f64::from(left)) as f32;
    let fy = (grid_y - f64::from(top)) as f32;
    let amplitude =
        3.2 * particle.opacity.clamp(0.0, 1.0) * (particle.radius.min(0.24) / 0.14).sqrt();
    for dy in 0..2 {
        for dx in 0..2 {
            let sub_x = left + dx;
            let sub_y = top + dy;
            let x = sub_x.div_euclid(2);
            let y = sub_y.div_euclid(4);
            if x < i32::from(area.x)
                || x >= i32::from(area.right())
                || y < i32::from(area.y)
                || y >= i32::from(area.bottom())
            {
                continue;
            }
            let key = (y as u16, x as u16);
            let weight = if dx == 0 { 1.0 - fx } else { fx } * if dy == 0 { 1.0 - fy } else { fy };
            let energy = amplitude * weight;
            if energy <= 0.0 {
                continue;
            }
            let index = sub_y.rem_euclid(4) as usize * 2 + sub_x.rem_euclid(2) as usize;
            let Some(field) = fields.field(key) else {
                continue;
            };
            field.has_embers = true;
            field.ember_energy[index] += energy;
            field.ember_thermal_energy[index] += energy * particle.heat.clamp(0.0, 1.0);
        }
    }
}

/// Rasterize with explicit particle, cell, and kernel-evaluation budgets. The
/// order of ordinary input particles does not affect overlap or glyph choices.
#[cfg(test)]
pub(crate) fn rasterize(area: Rect, particles: &[FireParticle]) -> Vec<FireCell> {
    let mut cells = Vec::new();
    rasterize_with(area, particles, |cell| cells.push(cell));
    cells
}

/// Stream cells through reusable per-thread scratch storage. A recursive
/// callback gets independent bounded scratch rather than borrowing the cache
/// twice; ordinary editor rendering performs no scratch allocation once warm.
pub(crate) fn rasterize_with(
    area: Rect,
    particles: &[FireParticle],
    mut emit: impl FnMut(FireCell),
) {
    if area.is_empty() || particles.is_empty() {
        return;
    }
    SCRATCH.with(|cache| {
        if let Ok(mut scratch) = cache.try_borrow_mut() {
            rasterize_cached(area, particles, &mut scratch, &mut emit);
        } else {
            rasterize_cached(area, particles, &mut RasterScratch::default(), &mut emit);
        }
    });
}

fn rasterize_cached(
    area: Rect,
    particles: &[FireParticle],
    fields: &mut RasterScratch,
    emit: &mut impl FnMut(FireCell),
) {
    fields.prepare(area);
    fields.particles.clear();
    fields.particles.extend(
        particles
            .iter()
            .take(MAX_PARTICLES)
            .enumerate()
            .filter_map(|(index, particle)| valid(particle).then_some(index)),
    );
    fields.particles.sort_unstable_by(|&first, &last| {
        let a = &particles[first];
        let b = &particles[last];
        a.seed
            .cmp(&b.seed)
            .then_with(|| a.position.x.total_cmp(&b.position.x))
            .then_with(|| a.position.y.total_cmp(&b.position.y))
            .then_with(|| a.velocity.x.total_cmp(&b.velocity.x))
            .then_with(|| a.velocity.y.total_cmp(&b.velocity.y))
            .then_with(|| a.heat.total_cmp(&b.heat))
            .then_with(|| a.opacity.total_cmp(&b.opacity))
            .then_with(|| a.radius.total_cmp(&b.radius))
            .then_with(|| a.kind.cmp(&b.kind))
    });
    let mut kernel = None;
    let mut work = 0;
    for index in 0..fields.particles.len() {
        let particle = &particles[fields.particles[index]];
        if particle.kind == FireParticleKind::Ember {
            if work + 4 > MAX_SAMPLE_WORK {
                return finish(fields, emit);
            }
            work += 4;
            splat_ember(area, particle, fields);
            continue;
        }
        let kernel = *kernel.get_or_insert_with(|| &*KERNEL);
        // Radius and velocity caps defend this pure renderer against malformed
        // synthetic input without changing the engine's ordinary small flames.
        let radius = f64::from(particle.radius).clamp(0.12, 2.0);
        let vx = f64::from(particle.velocity.x);
        let vy = f64::from(particle.velocity.y) * 2.0;
        let speed = vx.hypot(vy);
        let (direction_x, direction_y) = if speed > 0.001 {
            (vx / speed, vy / speed)
        } else {
            (0.0, -1.0)
        };
        let across = radius * 0.82;
        let along = radius * (1.15 + (speed * 0.018).min(1.35));
        let inverse_across_squared = (across * across).recip();
        let inverse_leading_squared = (along * 1.14).powi(2).recip();
        let inverse_trailing_squared = (along * 0.82).powi(2).recip();
        // The larger leading ellipse encloses both halves. Its row-wise
        // intersection skips empty subcell columns before evaluating kernels.
        let ellipse_x = direction_x * direction_x * inverse_leading_squared
            + direction_y * direction_y * inverse_across_squared;
        let ellipse_xy =
            direction_x * direction_y * (inverse_leading_squared - inverse_across_squared);
        let ellipse_y = direction_y * direction_y * inverse_leading_squared
            + direction_x * direction_x * inverse_across_squared;
        let longest = along * 1.14;
        let extent_x = (longest * direction_x).hypot(across * direction_y) * KERNEL_CUTOFF.sqrt();
        let extent_y =
            (longest * direction_y).hypot(across * direction_x) * KERNEL_CUTOFF.sqrt() * 0.5;
        let center_x = f64::from(particle.position.x);
        let center_y = f64::from(particle.position.y);
        if center_x + extent_x < f64::from(area.x)
            || center_x - extent_x >= f64::from(area.right())
            || center_y + extent_y < f64::from(area.y)
            || center_y - extent_y >= f64::from(area.bottom())
        {
            continue;
        }
        let left = (center_x - extent_x).floor().max(f64::from(area.x)) as u16;
        let right = (center_x + extent_x).ceil().min(f64::from(area.right())) as u16;
        let top = (center_y - extent_y).floor().max(f64::from(area.y)) as u16;
        let bottom = (center_y + extent_y).ceil().min(f64::from(area.bottom())) as u16;
        let heat = particle.heat.clamp(0.0, 1.0);
        let thermal_bias = 0.4 + 2.6 * heat.powi(3);
        let tiny_particle_energy = (f64::from(particle.radius) / 0.12).powi(2).min(1.0);
        let amplitude = 1.7 * f64::from(particle.opacity.clamp(0.0, 1.0)) * tiny_particle_energy;
        for y in top..bottom {
            let row_bounds: [Option<(f64, f64, f64)>; ROWS] = std::array::from_fn(|row| {
                let dy = (f64::from(y) + (row as f64 + 0.5) / ROWS as f64 - center_y) * 2.0;
                let linear = dy * ellipse_xy;
                let discriminant =
                    linear * linear - ellipse_x * (dy * dy * ellipse_y - KERNEL_CUTOFF);
                if discriminant <= 0.0 {
                    None
                } else {
                    let extent = discriminant.sqrt();
                    Some((
                        dy,
                        center_x + (-linear - extent) / ellipse_x,
                        center_x + (-linear + extent) / ellipse_x,
                    ))
                }
            });
            for x in left..right {
                if work + SAMPLES > MAX_SAMPLE_WORK {
                    return finish(fields, emit);
                }
                work += SAMPLES;
                let Some(field) = fields.field((y, x)) else {
                    continue;
                };
                for (row, &bounds) in row_bounds.iter().enumerate() {
                    let Some((dy, row_left, row_right)) = bounds else {
                        continue;
                    };
                    let first = (((row_left - f64::from(x)) * COLUMNS as f64 - 0.5).ceil() as i32)
                        .clamp(0, COLUMNS as i32) as usize;
                    let end = ((((row_right - f64::from(x)) * COLUMNS as f64 - 0.5).floor() as i32)
                        + 1)
                    .clamp(0, COLUMNS as i32) as usize;
                    for column in first..end {
                        let dx = f64::from(x) + (column as f64 + 0.5) / COLUMNS as f64 - center_x;
                        let axial = dx * direction_x + dy * direction_y;
                        let transverse = dx * direction_y - dy * direction_x;
                        // A longer leading half and narrower shoulder make a
                        // lick of flame. Its soft cutoff keeps subcell motion
                        // continuous as it enters or leaves a terminal cell.
                        let inverse_axial_squared = if axial >= 0.0 {
                            inverse_leading_squared
                        } else {
                            inverse_trailing_squared
                        };
                        let q = axial * axial * inverse_axial_squared
                            + transverse * transverse * inverse_across_squared;
                        if q >= KERNEL_CUTOFF {
                            continue;
                        }
                        let energy = (amplitude * f64::from(kernel_weight(q, kernel))) as f32;
                        let index = row * COLUMNS + column;
                        field.prepare_flame();
                        field.energy[index] += energy;
                        let weight = energy * thermal_bias;
                        field.thermal_weight[index] += weight;
                        field.thermal_energy[index] += weight * heat;
                    }
                }
            }
        }
    }
    finish(fields, emit)
}

fn best_glyph(light: &[f32; SAMPLES]) -> (Glyph, f32) {
    // All glyphs are unions of horizontal/vertical strips or quadrants. Sum
    // each row, column and quadrant once instead of revisiting up to 128
    // samples for each of the 39 candidates.
    let mut rows = [0.0; ROWS + 1];
    let mut columns = [0.0; COLUMNS + 1];
    let mut quadrants = [0.0; 4];
    for row in 0..ROWS {
        for column in 0..COLUMNS {
            let value = light[row * COLUMNS + column];
            rows[row + 1] += value;
            columns[column + 1] += value;
            quadrants[(row / (ROWS / 2)) * 2 + column / (COLUMNS / 2)] += value;
        }
    }
    for row in 1..=ROWS {
        rows[row] += rows[row - 1];
    }
    for column in 1..=COLUMNS {
        columns[column] += columns[column - 1];
    }
    let total = rows[ROWS];
    let mut sums = [0.0; GLYPHS.len()];
    sums[0] = total;
    for width in 1..8 {
        sums[width] = total - rows[ROWS - width * 2];
        sums[7 + width] = rows[width * 2];
        sums[14 + width] = columns[width];
        sums[21 + width] = columns[COLUMNS] - columns[COLUMNS - width];
    }
    sums[29..33].copy_from_slice(&quadrants);
    sums[33] = quadrants[0] + quadrants[3];
    sums[34] = quadrants[1] + quadrants[2];
    sums[35] = quadrants[0] + quadrants[1] + quadrants[2];
    sums[36] = quadrants[0] + quadrants[1] + quadrants[3];
    sums[37] = quadrants[0] + quadrants[2] + quadrants[3];
    sums[38] = quadrants[1] + quadrants[2] + quadrants[3];
    // Least-squares fitting chooses both shape and brightness. Keeping full
    // blocks first gives a stable, dim fringe when several shapes fit equally.
    let mut best = GLYPHS[0];
    let mut best_light = 0.0;
    let mut best_score = -1.0;
    for (candidate, sum) in GLYPHS.into_iter().zip(sums) {
        let average = sum / candidate.count as f32;
        let score = sum * average;
        if score > best_score + 0.000_001 {
            best = candidate;
            best_light = average;
            best_score = score;
        }
    }
    (best, best_light)
}

fn finish(fields: &mut RasterScratch, emit: &mut impl FnMut(FireCell)) {
    // Sort small indices, not three 128-float arrays per cell. Keep the sample
    // fields in their slots, including across frames with changing geometry.
    fields.order.clear();
    fields.order.extend(0..fields.used);
    fields.order.sort_unstable_by_key(|&index| fields.fields[index].0);
    let mut tone = None;
    for &index in &fields.order {
        let ((y, x), field) = &fields.fields[index];
        let (y, x) = (*y, *x);
        // Detached sparks do not need the much more expensive solid-shape
        // fit or the 128-sample flame tone map.
        let (light, glyph, opacity) = if field.has_flame {
            let tone = *tone.get_or_insert_with(|| &*TONE);
            let light = field.energy.map(|energy| tone_map(energy, tone));
            let (glyph, opacity) = best_glyph(&light);
            (light, glyph, opacity)
        } else {
            ([0.0; SAMPLES], GLYPHS[0], 0.0)
        };
        let flame_light = opacity * glyph.count as f32 / SAMPLES as f32;
        // Spark selection uses a hard visibility threshold. There are only
        // eight dots per cell, so keep their tone map exact at that edge.
        let sparks = if field.has_embers { field
            .ember_energy
            .map(|energy| -(-energy.min(16.0)).exp_m1()) } else { [0.0; 8] };
        let peak = sparks.into_iter().fold(0.0_f32, f32::max);
        if peak > 0.035 && flame_light < 0.14 + peak * 0.18 {
            // Unicode's dot order differs from our row-major 2×4 grid.
            const DOT_BITS: [u32; 8] = [0, 3, 1, 4, 2, 5, 6, 7];
            let threshold = (peak * 0.28).max(0.035);
            let mut mask = 0u32;
            let mut total = 0.0;
            let mut temperature = 0.0;
            for (index, spark) in sparks.into_iter().enumerate() {
                if spark >= threshold {
                    mask |= 1 << DOT_BITS[index];
                    total += spark;
                    temperature += spark * field.ember_thermal_energy[index]
                        / field.ember_energy[index].max(f32::MIN_POSITIVE);
                }
            }
            let count = mask.count_ones() as f32;
            emit(FireCell {
                x,
                y,
                symbol: char::from_u32(0x2800 + mask).unwrap(),
                inverse: false,
                coverage: count / 8.0 * 0.40,
                heat: (temperature / total).clamp(0.0, 1.0),
                opacity: (total / count).clamp(0.0, 1.0),
                glow: (flame_light * 0.30 + total / 8.0 * 0.08).min(0.14),
            });
            continue;
        }
        if opacity < 0.008 {
            continue;
        }
        let mut temperature = 0.0;
        let mut temperature_weight = 0.0;
        let mut mask = glyph.mask;
        while mask != 0 {
            let index = mask.trailing_zeros() as usize;
            if field.thermal_weight[index] > 0.0 {
                // The bright interior determines a cell's color; tiny cool
                // fringes cannot wash a hot flame center into dull orange.
                let weight = light[index].powi(2);
                temperature += weight * field.thermal_energy[index] / field.thermal_weight[index];
                temperature_weight += weight;
            }
            mask &= mask - 1;
        }
        emit(FireCell {
            x,
            y,
            symbol: glyph.symbol,
            inverse: glyph.inverse,
            coverage: glyph.count as f32 / SAMPLES as f32,
            heat: (temperature / temperature_weight.max(f32::MIN_POSITIVE)).clamp(0.0, 1.0),
            opacity: opacity.clamp(0.0, 1.0),
            glow: 0.0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animations::MotionPoint;

    fn particle(x: f32, y: f32) -> FireParticle {
        FireParticle {
            kind: FireParticleKind::Flame,
            position: MotionPoint { x, y },
            velocity: MotionPoint { x: 0.0, y: -3.0 },
            heat: 0.8,
            opacity: 0.8,
            radius: 0.65,
            seed: 7,
        }
    }

    fn luminance(cells: &[FireCell]) -> f32 {
        cells.iter().map(|cell| cell.coverage * cell.opacity).sum()
    }

    fn ember(x: f32, y: f32) -> FireParticle {
        FireParticle {
            kind: FireParticleKind::Ember,
            radius: 0.14,
            ..particle(x, y)
        }
    }

    #[test]
    fn tiny_embers_share_a_cell_without_merging_their_distinct_dot_positions() {
        let area = Rect::new(0, 0, 20, 10);
        let first = ember(5.25, 4.125);
        let mut second = ember(5.75, 4.875);
        second.heat = 0.3;
        second.seed = 12;
        let single = rasterize(area, &[first]);
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].symbol, '\u{2801}');
        let joined = rasterize(area, &[first, second]);
        assert_eq!(joined.len(), 1);
        assert_eq!(joined[0].symbol, '\u{2881}');
        assert!(joined[0].coverage < 0.15);
        assert!(joined[0].opacity > 0.5);
        assert!(joined[0].heat > 0.3 && joined[0].heat < 0.8);
        assert!(joined[0].glow > 0.0 && joined[0].glow < 0.1);
        assert_eq!(joined, rasterize(area, &[second, first]));
    }

    #[test]
    fn fine_sparks_cross_cell_boundaries_without_a_brightness_seam() {
        let area = Rect::new(0, 0, 20, 10);
        let left = rasterize(area, &[ember(5.98, 4.5)]);
        let right = rasterize(area, &[ember(6.02, 4.5)]);
        assert_eq!(left.len(), 2);
        assert_eq!(right.len(), 2);
        assert!((luminance(&left) - luminance(&right)).abs() < 0.000_1);
        assert_ne!(left, right);
        for cell in left.iter().chain(&right) {
            assert!(('\u{2801}'..='\u{28ff}').contains(&cell.symbol));
            assert!(!cell.inverse);
        }
    }

    #[test]
    fn fine_spark_visibility_stays_exact_at_its_tone_threshold() {
        let area = Rect::new(0, 0, 20, 10);
        for (light, expected) in [(0.035_000_1_f32, 1), (0.034_999_9_f32, 0)] {
            let mut spark = ember(5.25, 4.125);
            spark.opacity = -(-light).ln_1p() / 3.2;
            assert_eq!(rasterize(area, &[spark]).len(), expected);
        }
    }

    #[test]
    fn dense_flame_keeps_its_solid_body_when_sparks_pass_through_it() {
        let area = Rect::new(0, 0, 20, 10);
        let mut flame = particle(5.5, 4.5);
        flame.heat = 1.0;
        flame.radius = 1.0;
        let sparks = ember(5.25, 4.125);
        let cells = rasterize(area, &[flame, flame, sparks]);
        let body = cells
            .iter()
            .find(|cell| (cell.x, cell.y) == (5, 4))
            .unwrap();
        assert!(GLYPHS.iter().any(|glyph| glyph.symbol == body.symbol));
        assert!(body.heat > 0.99);
        assert_eq!(cells, rasterize(area, &[sparks, flame, flame]));
    }

    #[test]
    fn fire_raster_fractional_motion_keeps_energy_continuous_and_uses_solid_edges() {
        let area = Rect::new(0, 0, 20, 10);
        let first = rasterize(area, &[particle(5.2, 4.3)]);
        let next = rasterize(area, &[particle(5.22, 4.3)]);
        assert!(!first.is_empty());
        assert!(first.iter().any(|cell| cell.coverage < 1.0));
        let change = (luminance(&next) - luminance(&first)).abs() / luminance(&first);
        assert!(change < 0.08, "fractional step changed energy by {change}");
        assert_ne!(first, next);
        assert!(
            first
                .iter()
                .all(|cell| GLYPHS.iter().any(|g| g.symbol == cell.symbol))
        );
    }

    #[test]
    fn fire_raster_overlap_brightens_without_unbounded_additive_white() {
        let area = Rect::new(0, 0, 20, 10);
        let single = rasterize(area, &[particle(5.3, 4.4)]);
        let repeated: Vec<_> = (0..4).map(|_| particle(5.3, 4.4)).collect();
        let dense = rasterize(area, &repeated);
        assert!(luminance(&dense) > luminance(&single));
        assert!(luminance(&dense) < luminance(&single) * 4.0);
        assert!(
            dense
                .iter()
                .all(|cell| cell.opacity > 0.0 && cell.opacity < 1.0)
        );
        assert!(dense.iter().all(|cell| (cell.heat - 0.8).abs() < 0.000_1));
    }

    #[test]
    fn fire_raster_hot_center_retains_temperature_inside_cool_cloud() {
        let area = Rect::new(0, 0, 20, 10);
        let mut hot = particle(5.5, 4.5);
        hot.heat = 1.0;
        hot.radius = 0.5;
        let mut cool = particle(5.5, 4.5);
        cool.heat = 0.2;
        cool.radius = 1.6;
        let cells = rasterize(area, &[hot, cool]);
        let center = cells
            .iter()
            .find(|cell| cell.x == 5 && cell.y == 4)
            .unwrap();
        let fringe = cells
            .iter()
            .find(|cell| cell.x == 7 && cell.y == 4)
            .unwrap();
        assert!(
            center.heat > 0.65,
            "hot center was diluted: {}",
            center.heat
        );
        assert!(center.heat > fringe.heat + 0.3);
    }

    #[test]
    fn fire_raster_neighboring_particles_merge_into_a_connected_flame() {
        let area = Rect::new(0, 0, 20, 10);
        let first = particle(5.0, 4.5);
        let second = particle(6.0, 4.5);
        let cells = rasterize(area, &[first, second]);
        let row: Vec<_> = cells.iter().filter(|cell| cell.y == 4).collect();
        assert!(row.iter().any(|cell| cell.x == 5));
        assert!(row.iter().any(|cell| cell.x == 6));
        let edge_rows = |cell: &FireCell, column: usize| {
            let glyph = GLYPHS
                .iter()
                .find(|glyph| glyph.symbol == cell.symbol && glyph.inverse == cell.inverse)
                .unwrap();
            (0..ROWS).fold(0u16, |rows, row| {
                rows | (((glyph.mask >> (row * COLUMNS + column)) & 1) as u16) << row
            })
        };
        for pair in row.windows(2) {
            assert_eq!(pair[1].x, pair[0].x + 1);
            // Adjacency of occupied cells alone is insufficient: their solid
            // subcell silhouettes must actually meet across each shared edge.
            assert_ne!(edge_rows(pair[0], COLUMNS - 1) & edge_rows(pair[1], 0), 0);
        }
        assert!(cells.len() > 2);
        let joined = row.iter().find(|cell| cell.x == 5).unwrap();
        let single_light = [first, second]
            .map(|particle| {
                rasterize(area, &[particle])
                    .into_iter()
                    .find(|cell| cell.x == 5 && cell.y == 4)
                    .map_or(0.0, |cell| cell.coverage * cell.opacity)
            })
            .into_iter()
            .fold(0.0, f32::max);
        // Contributions merge in the field before fitting a glyph. The cell
        // between the sources becomes a substantial bright flame shoulder,
        // while its upper fractional block still follows the rising contour.
        assert!(joined.coverage >= 0.5);
        assert!(joined.coverage * joined.opacity > single_light * 1.5);
    }

    #[test]
    fn fire_raster_overlap_is_independent_of_particle_order() {
        let first = particle(5.2, 4.4);
        let mut second = particle(5.5, 4.2);
        second.seed = 12;
        second.heat = 0.3;
        let mut third = particle(5.6, 4.7);
        third.seed = 7;
        let area = Rect::new(0, 0, 20, 10);
        assert_eq!(
            rasterize(area, &[first, second, third]),
            rasterize(area, &[third, second, first])
        );
    }

    #[test]
    fn fire_raster_glyph_fitting_covers_fractional_and_inverse_edges() {
        for expected in GLYPHS {
            let light = std::array::from_fn(|index| {
                if expected.mask & (1u128 << index) != 0 {
                    0.7
                } else {
                    0.0
                }
            });
            let (actual, opacity) = best_glyph(&light);
            assert_eq!(actual.mask, expected.mask);
            assert!((opacity - 0.7).abs() < 0.000_01);
        }
    }

    #[test]
    fn summed_glyph_fit_matches_independent_mask_sums() {
        let mut seed = 0x973a_b231_u32;
        for _ in 0..2_000 {
            let light = std::array::from_fn(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 8) as f32 / 16_777_216.0
            });
            let mut expected = GLYPHS[0];
            let mut expected_light = 0.0;
            let mut best_score = -1.0;
            for candidate in GLYPHS {
                let sum: f32 = light
                    .iter()
                    .enumerate()
                    .filter_map(|(index, &value)| {
                        (candidate.mask & (1u128 << index) != 0).then_some(value)
                    })
                    .sum();
                let average = sum / candidate.count as f32;
                let score = sum * average;
                if score > best_score + 0.000_001 {
                    expected = candidate;
                    expected_light = average;
                    best_score = score;
                }
            }
            let (actual, actual_light) = best_glyph(&light);
            assert_eq!(actual.mask, expected.mask);
            assert!((actual_light - expected_light).abs() < 0.000_002);
        }
    }

    #[test]
    fn interpolated_kernel_and_tone_tables_stay_below_visible_precision() {
        let kernel = &*KERNEL;
        let tone = &*TONE;
        for index in 0..100_000 {
            let q = index as f64 * KERNEL_CUTOFF / 100_000.0;
            let expected =
                ((-1.6 * q).exp() * (1.0 - smoothstep((q - 1.5) / (KERNEL_CUTOFF - 1.5)))) as f32;
            assert!((kernel_weight(q, kernel) - expected).abs() < 0.000_001);
            let energy = index as f32 * 16.0 / 99_999.0;
            let expected = -(-energy.min(16.0)).exp_m1();
            assert!((tone_map(energy, tone) - expected).abs() < 0.000_002);
        }
        assert_eq!(tone_map(0.0, tone), 0.0);
        assert_eq!(tone_map(16.0, tone), tone_map(f32::MAX, tone));
    }

    #[test]
    fn dense_fire_index_matches_sparse_across_resizes_and_effect_changes() {
        let mut dense = RasterScratch::default();
        let mut sparse = RasterScratch::default();
        for step in 0..60 {
            let area = match step % 5 {
                0 => Rect::new(0, 0, 100, 30),
                1 => Rect::new(3, 2, 16, 12),
                2 => Rect::new(0, 0, 500, 200), // bounded sparse fallback
                3 => Rect::new(11, 5, 7, 4),
                _ => Rect::new(0, 0, 1, 1),
            };
            let input: Vec<_> = (0..45).map(|i| {
                let x = (i * 7 % 21) as f32 + step as f32 * 0.013;
                let y = (i * 3 % 13) as f32 + 0.7;
                if (i + step) % 3 == 0 { ember(x, y) } else { particle(x, y) }
            }).collect();
            let mut actual = Vec::new();
            let mut expected = Vec::new();
            REFERENCE_INDEX.set(false);
            rasterize_cached(area, &input, &mut dense, &mut |cell| actual.push(cell));
            REFERENCE_INDEX.set(true);
            rasterize_cached(area, &input, &mut sparse, &mut |cell| expected.push(cell));
            REFERENCE_INDEX.set(false);
            assert_eq!(actual, expected, "step={step}");
            assert!(actual.windows(2).all(|pair| (pair[0].y, pair[0].x) < (pair[1].y, pair[1].x)));
            assert!(dense.slots.len() <= MAX_INDEX_CELLS);
            assert!(dense.used <= MAX_CELLS);
        }
    }

    #[test]
    fn sorting_fire_output_does_not_move_sample_arrays_or_leave_old_slots_live() {
        let mut scratch = RasterScratch::default();
        scratch.prepare(Rect::new(0, 0, 30, 20));
        for key in [(10, 8), (0, 1), (4, 19)] {
            scratch.field(key).unwrap().has_embers = true;
        }
        let keys: Vec<_> = scratch.fields.iter().map(|entry| entry.0).collect();
        let pointer = scratch.fields.as_ptr();
        finish(&mut scratch, &mut |_| {});
        assert_eq!(scratch.fields.as_ptr(), pointer);
        assert_eq!(scratch.fields.iter().map(|entry| entry.0).collect::<Vec<_>>(), keys);
        assert_eq!(scratch.order, [1, 2, 0]);
        scratch.prepare(Rect::new(2, 3, 20, 15));
        assert!(scratch.slots.iter().all(|&slot| slot == 0));
        let field = scratch.field((10, 8)).unwrap();
        assert!(!field.has_flame && !field.has_embers);
        assert!(field.ember_energy.iter().all(|&energy| energy == 0.0));
    }

    #[test]
    fn warmed_streaming_raster_reuses_all_scratch_storage() {
        let area = Rect::new(0, 0, 20, 10);
        let input = [particle(5.2, 4.4), ember(5.7, 4.6)];
        let expected = rasterize(area, &input);
        let capacities = SCRATCH.with(|cache| {
            let scratch = cache.borrow();
            (
                scratch.particles.capacity(),
                scratch.index.capacity(),
                scratch.fields.capacity(),
                scratch.slots.capacity(),
                scratch.order.capacity(),
            )
        });
        for _ in 0..20 {
            let mut actual = Vec::new();
            rasterize_with(area, &input, |cell| actual.push(cell));
            assert_eq!(actual, expected);
        }
        SCRATCH.with(|cache| {
            let scratch = cache.borrow();
            assert_eq!(
                capacities,
                (
                    scratch.particles.capacity(),
                    scratch.index.capacity(),
                    scratch.fields.capacity(),
                    scratch.slots.capacity(),
                    scratch.order.capacity(),
                )
            );
        });
    }

    #[test]
    fn streaming_callbacks_can_render_nested_fire_without_losing_outer_fields() {
        let area = Rect::new(0, 0, 20, 10);
        let input = [particle(5.2, 4.4)];
        let expected = rasterize(area, &input);
        let mut actual = Vec::new();
        rasterize_with(area, &input, |cell| {
            assert_eq!(rasterize(area, &input), expected);
            actual.push(cell);
        });
        assert_eq!(actual, expected);
    }

    #[test]
    fn fire_raster_invalid_and_offscreen_particles_do_not_emit() {
        let area = Rect::new(2, 3, 10, 5);
        let mut invalid = Vec::new();
        for number in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut p = particle(5.0, 4.0);
            p.position.x = number;
            invalid.push(p);
            let mut p = particle(5.0, 4.0);
            p.radius = number;
            invalid.push(p);
            let mut p = particle(5.0, 4.0);
            p.velocity.y = number;
            invalid.push(p);
            let mut p = particle(5.0, 4.0);
            p.heat = number;
            invalid.push(p);
        }
        invalid.push(particle(f32::MAX, f32::MAX));
        assert!(rasterize(area, &invalid).is_empty());
        assert!(rasterize(Rect::default(), &[particle(5.0, 4.0)]).is_empty());
        let cells = rasterize(area, &[particle(2.0, 3.0)]);
        assert!(!cells.is_empty());
        assert!(
            cells
                .iter()
                .all(|cell| area.contains((cell.x, cell.y).into()))
        );
    }

    #[test]
    fn fire_raster_huge_inputs_stay_finite_clipped_and_bounded() {
        let area = Rect::new(0, 0, u16::MAX, u16::MAX);
        let particles: Vec<_> = (0..MAX_PARTICLES * 3)
            .map(|index| {
                let mut p = particle((index % 100) as f32 * 10.0, (index / 100) as f32 * 5.0);
                p.radius = f32::MAX;
                p.velocity.x = f32::MAX;
                p.velocity.y = -f32::MAX;
                p.seed = index as u32;
                if index % 2 == 0 {
                    p.kind = FireParticleKind::Ember;
                }
                p
            })
            .collect();
        let cells = rasterize(area, &particles);
        assert!(!cells.is_empty());
        assert!(cells.len() <= MAX_CELLS);
        assert!(cells.iter().all(|cell| cell.opacity.is_finite()
            && cell.heat.is_finite()
            && cell.glow.is_finite()
            && (0.0..=1.0).contains(&cell.opacity)
            && (0.0..=1.0).contains(&cell.heat)
            && (0.0..=1.0).contains(&cell.coverage)));
    }

    #[test]
    #[ignore = "measures real Fire Hazard particle rasterization"]
    fn profile_fire_hazard_rendering() {
        use crate::{
            animation_config::AnimationSettings, cursor_fire::FireTrail, cursor_smear::SmearMotion,
        };
        use std::time::{Duration, Instant};
        for intensity in [55, 100] {
            let mut settings = AnimationSettings::default();
            settings.intensity = intensity;
            let start = Instant::now();
            let mut fire = FireTrail::default();
            fire.retarget(
                SmearMotion::retarget(
                    MotionPoint { x: 4.0, y: 12.0 },
                    MotionPoint { x: 64.0, y: 12.0 },
                    None,
                    false,
                ),
                start,
                false,
                &settings,
            );
            let area = Rect::new(0, 0, 100, 30);
            let mut elapsed = Vec::new();
            let mut max_particles = 0;
            let mut max_cells = 0;
            for milliseconds in [40, 80, 160, 320, 600, 900] {
                let now = start + Duration::from_millis(milliseconds);
                fire.advance(now, &settings);
                let particles = fire.sample(now);
                max_particles = max_particles.max(particles.len());
                for _ in 0..100 {
                    let before = Instant::now();
                    let cells = std::hint::black_box(rasterize(area, &particles));
                    elapsed.push(before.elapsed().as_secs_f64() * 1_000.0);
                    max_cells = max_cells.max(cells.len());
                }
            }
            elapsed.sort_by(f64::total_cmp);
            eprintln!(
                "Fire intensity {intensity}: p50 {:.3} ms, p95 {:.3} ms; peak {max_particles} particles, {max_cells} cells",
                elapsed[elapsed.len() / 2],
                elapsed[elapsed.len() * 95 / 100],
            );
        }
    }
}
