use super::contracts::{validate_identifier, VideoError, VideoErrorCode, VideoResult};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const RTX_4080_LAPTOP_VRAM_MB: u32 = 12_282;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceClass {
    Light,
    Medium,
    Heavy,
    Exclusive,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceCapacity {
    pub vram_mb: u32,
    pub cpu_threads: u16,
    pub io_slots: u16,
    pub nvenc_sessions: u8,
    pub max_medium_jobs: u16,
    pub max_heavy_jobs: u16,
}

impl ResourceCapacity {
    /// Calibrated admission envelope for the target RTX 4080 Laptop / i9-13900HX host.
    /// The scheduler accounts the full physical VRAM figure; individual profiles retain
    /// their own safety margin instead of hiding it in a second capacity number.
    pub const fn rtx_4080_laptop() -> Self {
        Self {
            vram_mb: RTX_4080_LAPTOP_VRAM_MB,
            cpu_threads: 32,
            io_slots: 8,
            nvenc_sessions: 2,
            max_medium_jobs: 2,
            max_heavy_jobs: 1,
        }
    }

    pub fn validate(self) -> VideoResult<()> {
        if self.vram_mb == 0
            || self.cpu_threads == 0
            || self.io_slots == 0
            || self.nvenc_sessions == 0
            || self.max_medium_jobs == 0
            || self.max_heavy_jobs == 0
        {
            return Err(VideoError::new(
                VideoErrorCode::InvalidResourceRequest,
                "scheduler capacity fields must all be positive",
            ));
        }
        Ok(())
    }
}

impl Default for ResourceCapacity {
    fn default() -> Self {
        Self::rtx_4080_laptop()
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceUsage {
    pub vram_mb: u32,
    pub cpu_threads: u16,
    pub io_slots: u16,
    pub nvenc_sessions: u8,
    pub light_jobs: u16,
    pub medium_jobs: u16,
    pub heavy_jobs: u16,
    pub exclusive_jobs: u16,
}

impl ResourceUsage {
    pub fn total_jobs(self) -> u32 {
        u32::from(self.light_jobs)
            + u32::from(self.medium_jobs)
            + u32::from(self.heavy_jobs)
            + u32::from(self.exclusive_jobs)
    }

    fn checked_add(self, request: ResourceRequest) -> VideoResult<Self> {
        let mut next = Self {
            vram_mb: self
                .vram_mb
                .checked_add(request.vram_mb)
                .ok_or_else(resource_arithmetic_overflow)?,
            cpu_threads: self
                .cpu_threads
                .checked_add(request.cpu_threads)
                .ok_or_else(resource_arithmetic_overflow)?,
            io_slots: self
                .io_slots
                .checked_add(request.io_slots)
                .ok_or_else(resource_arithmetic_overflow)?,
            nvenc_sessions: self
                .nvenc_sessions
                .checked_add(request.nvenc_sessions)
                .ok_or_else(resource_arithmetic_overflow)?,
            ..self
        };
        let class_counter = match request.class {
            ResourceClass::Light => &mut next.light_jobs,
            ResourceClass::Medium => &mut next.medium_jobs,
            ResourceClass::Heavy => &mut next.heavy_jobs,
            ResourceClass::Exclusive => &mut next.exclusive_jobs,
        };
        *class_counter = class_counter
            .checked_add(1)
            .ok_or_else(resource_arithmetic_overflow)?;
        Ok(next)
    }

    fn checked_sub(self, request: ResourceRequest) -> VideoResult<Self> {
        let mut next = Self {
            vram_mb: self
                .vram_mb
                .checked_sub(request.vram_mb)
                .ok_or_else(resource_arithmetic_overflow)?,
            cpu_threads: self
                .cpu_threads
                .checked_sub(request.cpu_threads)
                .ok_or_else(resource_arithmetic_overflow)?,
            io_slots: self
                .io_slots
                .checked_sub(request.io_slots)
                .ok_or_else(resource_arithmetic_overflow)?,
            nvenc_sessions: self
                .nvenc_sessions
                .checked_sub(request.nvenc_sessions)
                .ok_or_else(resource_arithmetic_overflow)?,
            ..self
        };
        let class_counter = match request.class {
            ResourceClass::Light => &mut next.light_jobs,
            ResourceClass::Medium => &mut next.medium_jobs,
            ResourceClass::Heavy => &mut next.heavy_jobs,
            ResourceClass::Exclusive => &mut next.exclusive_jobs,
        };
        *class_counter = class_counter
            .checked_sub(1)
            .ok_or_else(resource_arithmetic_overflow)?;
        Ok(next)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceRequest {
    pub class: ResourceClass,
    pub vram_mb: u32,
    pub cpu_threads: u16,
    pub io_slots: u16,
    pub nvenc_sessions: u8,
}

impl ResourceRequest {
    pub const fn light() -> Self {
        Self {
            class: ResourceClass::Light,
            vram_mb: 0,
            cpu_threads: 2,
            io_slots: 1,
            nvenc_sessions: 0,
        }
    }

    pub const fn medium_nvenc() -> Self {
        Self {
            class: ResourceClass::Medium,
            vram_mb: 1_024,
            cpu_threads: 4,
            io_slots: 2,
            nvenc_sessions: 1,
        }
    }

    pub const fn heavy_inference() -> Self {
        Self {
            class: ResourceClass::Heavy,
            vram_mb: 6_144,
            cpu_threads: 8,
            io_slots: 2,
            nvenc_sessions: 0,
        }
    }

    pub const fn exclusive_gpu() -> Self {
        Self {
            class: ResourceClass::Exclusive,
            vram_mb: 11_500,
            cpu_threads: 16,
            io_slots: 4,
            nvenc_sessions: 1,
        }
    }

    pub fn validate(self, capacity: ResourceCapacity) -> VideoResult<()> {
        capacity.validate()?;
        if self.cpu_threads == 0 || self.io_slots == 0 {
            return Err(VideoError::new(
                VideoErrorCode::InvalidResourceRequest,
                "every job must reserve at least one CPU thread and I/O slot",
            ));
        }
        if self.vram_mb > capacity.vram_mb
            || self.cpu_threads > capacity.cpu_threads
            || self.io_slots > capacity.io_slots
            || self.nvenc_sessions > capacity.nvenc_sessions
        {
            return Err(VideoError::new(
                VideoErrorCode::InvalidResourceRequest,
                "resource request exceeds scheduler capacity",
            ));
        }
        let within_class_envelope = match self.class {
            ResourceClass::Light => {
                self.vram_mb <= 1_024
                    && self.cpu_threads <= 4
                    && self.io_slots <= 2
                    && self.nvenc_sessions == 0
            }
            ResourceClass::Medium => {
                self.vram_mb <= 4_096
                    && self.cpu_threads <= 8
                    && self.io_slots <= 3
                    && self.nvenc_sessions <= 1
            }
            ResourceClass::Heavy => {
                self.vram_mb <= 9_216
                    && self.cpu_threads <= 16
                    && self.io_slots <= 4
                    && self.nvenc_sessions <= 2
            }
            ResourceClass::Exclusive => true,
        };
        if !within_class_envelope {
            return Err(VideoError::new(
                VideoErrorCode::InvalidResourceRequest,
                "request exceeds its declared resource-class envelope",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionBlock {
    ExclusiveActive,
    ExclusiveRequiresIdle,
    VramCapacity,
    CpuCapacity,
    IoCapacity,
    NvencCapacity,
    MediumConcurrency,
    HeavyConcurrency,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceLease {
    pub job_id: String,
    pub request: ResourceRequest,
    pub admission_sequence: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdmissionOutcome {
    Admitted(ResourceLease),
    Waiting { blocks: Vec<AdmissionBlock> },
}

#[derive(Clone, Debug)]
pub struct ResourceScheduler {
    capacity: ResourceCapacity,
    usage: ResourceUsage,
    active: BTreeMap<String, ResourceLease>,
    next_sequence: u64,
}

impl ResourceScheduler {
    pub fn new(capacity: ResourceCapacity) -> VideoResult<Self> {
        capacity.validate()?;
        Ok(Self {
            capacity,
            usage: ResourceUsage::default(),
            active: BTreeMap::new(),
            next_sequence: 1,
        })
    }

    pub fn for_rtx_4080_laptop() -> Self {
        Self::new(ResourceCapacity::rtx_4080_laptop())
            .expect("the built-in RTX 4080 resource profile is valid")
    }

    pub fn capacity(&self) -> ResourceCapacity {
        self.capacity
    }

    pub fn usage(&self) -> ResourceUsage {
        self.usage
    }

    pub fn available(&self) -> ResourceUsage {
        ResourceUsage {
            vram_mb: self.capacity.vram_mb - self.usage.vram_mb,
            cpu_threads: self.capacity.cpu_threads - self.usage.cpu_threads,
            io_slots: self.capacity.io_slots - self.usage.io_slots,
            nvenc_sessions: self.capacity.nvenc_sessions - self.usage.nvenc_sessions,
            ..ResourceUsage::default()
        }
    }

    pub fn active_lease(&self, job_id: &str) -> Option<&ResourceLease> {
        self.active.get(job_id)
    }

    pub fn active_leases(&self) -> impl Iterator<Item = &ResourceLease> {
        self.active.values()
    }

    /// Atomically checks accounting and records a lease. `Waiting` is normal backpressure;
    /// malformed or duplicate jobs are returned as stable VideoError values.
    pub fn try_acquire(
        &mut self,
        job_id: impl Into<String>,
        request: ResourceRequest,
    ) -> VideoResult<AdmissionOutcome> {
        let job_id = job_id.into();
        validate_identifier(&job_id, "scheduler.job_id")?;
        request.validate(self.capacity)?;
        if self.active.contains_key(&job_id) {
            return Err(VideoError::new(
                VideoErrorCode::JobAlreadyActive,
                format!("job {job_id} already owns a resource lease"),
            ));
        }
        let blocks = self.admission_blocks(request)?;
        if !blocks.is_empty() {
            return Ok(AdmissionOutcome::Waiting { blocks });
        }

        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.checked_add(1).ok_or_else(|| {
            VideoError::new(
                VideoErrorCode::ArithmeticOverflow,
                "resource admission sequence overflowed",
            )
        })?;
        let lease = ResourceLease {
            job_id: job_id.clone(),
            request,
            admission_sequence: sequence,
        };
        self.usage = self.usage.checked_add(request)?;
        self.active.insert(job_id, lease.clone());
        Ok(AdmissionOutcome::Admitted(lease))
    }

    /// Idempotent release is useful for cancellation and crash-recovery cleanup.
    pub fn release(&mut self, job_id: &str) -> VideoResult<Option<ResourceLease>> {
        let Some(lease) = self.active.remove(job_id) else {
            return Ok(None);
        };
        self.usage = self.usage.checked_sub(lease.request)?;
        Ok(Some(lease))
    }

    pub fn admission_blocks(&self, request: ResourceRequest) -> VideoResult<Vec<AdmissionBlock>> {
        request.validate(self.capacity)?;
        let mut blocks = Vec::new();
        if self.usage.exclusive_jobs > 0 {
            blocks.push(AdmissionBlock::ExclusiveActive);
        }
        if matches!(request.class, ResourceClass::Exclusive) && self.usage.total_jobs() > 0 {
            blocks.push(AdmissionBlock::ExclusiveRequiresIdle);
        }
        let projected = self.usage.checked_add(request)?;
        if projected.vram_mb > self.capacity.vram_mb {
            blocks.push(AdmissionBlock::VramCapacity);
        }
        if projected.cpu_threads > self.capacity.cpu_threads {
            blocks.push(AdmissionBlock::CpuCapacity);
        }
        if projected.io_slots > self.capacity.io_slots {
            blocks.push(AdmissionBlock::IoCapacity);
        }
        if projected.nvenc_sessions > self.capacity.nvenc_sessions {
            blocks.push(AdmissionBlock::NvencCapacity);
        }
        if projected.medium_jobs > self.capacity.max_medium_jobs {
            blocks.push(AdmissionBlock::MediumConcurrency);
        }
        if projected.heavy_jobs > self.capacity.max_heavy_jobs {
            blocks.push(AdmissionBlock::HeavyConcurrency);
        }
        blocks.sort_unstable();
        blocks.dedup();
        Ok(blocks)
    }
}

impl Default for ResourceScheduler {
    fn default() -> Self {
        Self::for_rtx_4080_laptop()
    }
}

fn resource_arithmetic_overflow() -> VideoError {
    VideoError::new(
        VideoErrorCode::ArithmeticOverflow,
        "resource accounting overflowed or underflowed",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admitted(outcome: AdmissionOutcome) -> ResourceLease {
        match outcome {
            AdmissionOutcome::Admitted(lease) => lease,
            AdmissionOutcome::Waiting { blocks } => panic!("unexpected wait: {blocks:?}"),
        }
    }

    fn waiting(outcome: AdmissionOutcome) -> Vec<AdmissionBlock> {
        match outcome {
            AdmissionOutcome::Waiting { blocks } => blocks,
            AdmissionOutcome::Admitted(lease) => panic!("unexpected lease: {lease:?}"),
        }
    }

    #[test]
    fn target_capacity_matches_physical_vram_and_cpu() {
        let capacity = ResourceCapacity::rtx_4080_laptop();
        assert_eq!(capacity.vram_mb, 12_282);
        assert_eq!(capacity.cpu_threads, 32);
        assert_eq!(capacity.nvenc_sessions, 2);
    }

    #[test]
    fn nvenc_preview_can_overlap_heavy_inference_within_vram() {
        let mut scheduler = ResourceScheduler::default();
        admitted(
            scheduler
                .try_acquire("whisper", ResourceRequest::heavy_inference())
                .unwrap(),
        );
        admitted(
            scheduler
                .try_acquire("preview", ResourceRequest::medium_nvenc())
                .unwrap(),
        );
        assert_eq!(scheduler.usage().vram_mb, 7_168);
        assert_eq!(scheduler.usage().nvenc_sessions, 1);
        assert_eq!(scheduler.usage().heavy_jobs, 1);
        assert_eq!(scheduler.usage().medium_jobs, 1);
    }

    #[test]
    fn second_heavy_job_waits_even_when_raw_vram_would_fit() {
        let mut scheduler = ResourceScheduler::default();
        admitted(
            scheduler
                .try_acquire("whisper", ResourceRequest::heavy_inference())
                .unwrap(),
        );
        let blocks = waiting(
            scheduler
                .try_acquire("music", ResourceRequest::heavy_inference())
                .unwrap(),
        );
        assert!(blocks.contains(&AdmissionBlock::HeavyConcurrency));
        assert!(blocks.contains(&AdmissionBlock::VramCapacity));
    }

    #[test]
    fn nvenc_sessions_are_accounted_independently() {
        let mut scheduler = ResourceScheduler::default();
        admitted(
            scheduler
                .try_acquire("preview-a", ResourceRequest::medium_nvenc())
                .unwrap(),
        );
        admitted(
            scheduler
                .try_acquire("preview-b", ResourceRequest::medium_nvenc())
                .unwrap(),
        );
        let blocks = waiting(
            scheduler
                .try_acquire(
                    "final",
                    ResourceRequest {
                        class: ResourceClass::Heavy,
                        vram_mb: 2_048,
                        cpu_threads: 8,
                        io_slots: 2,
                        nvenc_sessions: 1,
                    },
                )
                .unwrap(),
        );
        assert!(blocks.contains(&AdmissionBlock::NvencCapacity));
    }

    #[test]
    fn exclusive_job_waits_for_idle_then_blocks_every_class() {
        let mut scheduler = ResourceScheduler::default();
        admitted(
            scheduler
                .try_acquire("probe", ResourceRequest::light())
                .unwrap(),
        );
        let blocks = waiting(
            scheduler
                .try_acquire("exclusive", ResourceRequest::exclusive_gpu())
                .unwrap(),
        );
        assert_eq!(blocks, vec![AdmissionBlock::ExclusiveRequiresIdle]);
        scheduler.release("probe").unwrap();
        admitted(
            scheduler
                .try_acquire("exclusive", ResourceRequest::exclusive_gpu())
                .unwrap(),
        );
        let blocks = waiting(
            scheduler
                .try_acquire("metadata", ResourceRequest::light())
                .unwrap(),
        );
        assert!(blocks.contains(&AdmissionBlock::ExclusiveActive));
    }

    #[test]
    fn release_restores_every_account_and_is_idempotent() {
        let mut scheduler = ResourceScheduler::default();
        admitted(
            scheduler
                .try_acquire("render", ResourceRequest::medium_nvenc())
                .unwrap(),
        );
        let lease = scheduler.release("render").unwrap().unwrap();
        assert_eq!(lease.job_id, "render");
        assert_eq!(scheduler.usage(), ResourceUsage::default());
        assert_eq!(scheduler.release("render").unwrap(), None);
    }

    #[test]
    fn duplicate_and_misclassified_requests_have_stable_errors() {
        let mut scheduler = ResourceScheduler::default();
        admitted(
            scheduler
                .try_acquire("job-1", ResourceRequest::light())
                .unwrap(),
        );
        let duplicate = scheduler
            .try_acquire("job-1", ResourceRequest::light())
            .unwrap_err();
        assert_eq!(duplicate.code, VideoErrorCode::JobAlreadyActive);

        let oversized_light = ResourceRequest {
            class: ResourceClass::Light,
            vram_mb: 6_000,
            cpu_threads: 2,
            io_slots: 1,
            nvenc_sessions: 0,
        };
        let invalid = scheduler
            .try_acquire("bad-job", oversized_light)
            .unwrap_err();
        assert_eq!(invalid.code, VideoErrorCode::InvalidResourceRequest);
    }
}
