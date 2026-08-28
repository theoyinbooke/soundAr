use super::contracts::{
    Microseconds, RationalFrameRate, TimeRange, TimelineClip, TimelineGap, TimelineTrack, Validate,
    VideoError, VideoErrorCode, VideoResult,
};

const MICROS_PER_SECOND: i128 = 1_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuantizeMode {
    Floor,
    Nearest,
    Ceil,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FramePoint {
    pub frame_index: i64,
    /// The selected frame boundary represented on the integer-microsecond manifest clock.
    pub time_us: Microseconds,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameRange {
    pub start_frame: i64,
    pub end_frame: i64,
    pub start_us: Microseconds,
    pub end_us: Microseconds,
}

impl FrameRange {
    pub fn frame_count(self) -> i64 {
        self.end_frame - self.start_frame
    }
}

/// Converts an integer-microsecond clock value to a frame index using exact i128 arithmetic.
/// No floating-point values enter timeline decisions, including 30000/1001 timebases.
pub fn frame_index_at(
    time_us: Microseconds,
    frame_rate: RationalFrameRate,
    mode: QuantizeMode,
) -> VideoResult<i64> {
    frame_rate.validate()?;
    if time_us.0 < 0 {
        return Err(VideoError::new(
            VideoErrorCode::InvalidTimestamp,
            "frame quantization requires a non-negative timestamp",
        ));
    }
    let numerator = (time_us.0 as i128)
        .checked_mul(frame_rate.numerator as i128)
        .ok_or_else(arithmetic_overflow)?;
    let denominator = MICROS_PER_SECOND
        .checked_mul(frame_rate.denominator as i128)
        .ok_or_else(arithmetic_overflow)?;
    checked_i128_to_i64(divide_nonnegative(numerator, denominator, mode)?)
}

/// Returns the selected frame boundary on the integer-microsecond manifest clock.
pub fn frame_time_us(
    frame_index: i64,
    frame_rate: RationalFrameRate,
    mode: QuantizeMode,
) -> VideoResult<Microseconds> {
    frame_rate.validate()?;
    if frame_index < 0 {
        return Err(VideoError::new(
            VideoErrorCode::InvalidTimestamp,
            "frame index must be non-negative",
        ));
    }
    let numerator = (frame_index as i128)
        .checked_mul(MICROS_PER_SECOND)
        .and_then(|value| value.checked_mul(frame_rate.denominator as i128))
        .ok_or_else(arithmetic_overflow)?;
    let time = divide_nonnegative(numerator, frame_rate.numerator as i128, mode)?;
    Ok(Microseconds(checked_i128_to_i64(time)?))
}

pub fn quantize_to_frame(
    time_us: Microseconds,
    frame_rate: RationalFrameRate,
    mode: QuantizeMode,
) -> VideoResult<FramePoint> {
    let frame_index = frame_index_at(time_us, frame_rate, mode)?;
    // Nearest produces the most faithful integer-microsecond serialization of a rational
    // frame boundary. The index itself was selected with the caller's requested mode.
    let time_us = frame_time_us(frame_index, frame_rate, QuantizeMode::Nearest)?;
    Ok(FramePoint {
        frame_index,
        time_us,
    })
}

/// Expands a time range to whole frames, never trimming source content.
pub fn quantize_range_outward(
    range: TimeRange,
    frame_rate: RationalFrameRate,
) -> VideoResult<FrameRange> {
    range.validate()?;
    let start_frame = frame_index_at(range.start_us, frame_rate, QuantizeMode::Floor)?;
    let end_frame = frame_index_at(range.end_us, frame_rate, QuantizeMode::Ceil)?;
    Ok(FrameRange {
        start_frame,
        end_frame,
        start_us: frame_time_us(start_frame, frame_rate, QuantizeMode::Nearest)?,
        end_us: frame_time_us(end_frame, frame_rate, QuantizeMode::Nearest)?,
    })
}

/// Maps a timeline timestamp into a clip's original source clock. Playback rate is
/// `source / timeline`, so 2/1 consumes two source microseconds per timeline microsecond.
pub fn map_timeline_to_source(
    clip: &TimelineClip,
    timeline_us: Microseconds,
    mode: QuantizeMode,
) -> VideoResult<Microseconds> {
    map_timeline_to_source_inner(clip, timeline_us, mode, false)
}

/// Converts a timeline edit boundary, including a clip's exclusive end, to the
/// corresponding source-clock boundary. This is deliberately separate from
/// [`map_timeline_to_source`], whose half-open semantics locate media samples.
pub fn map_timeline_endpoint_to_source(
    clip: &TimelineClip,
    timeline_us: Microseconds,
    mode: QuantizeMode,
) -> VideoResult<Microseconds> {
    map_timeline_to_source_inner(clip, timeline_us, mode, true)
}

fn map_timeline_to_source_inner(
    clip: &TimelineClip,
    timeline_us: Microseconds,
    mode: QuantizeMode,
    endpoint: bool,
) -> VideoResult<Microseconds> {
    clip.validate()?;
    let timeline_range = clip.timeline_range()?;
    let in_range = if endpoint {
        timeline_range.contains_endpoint(timeline_us)
    } else {
        timeline_range.contains(timeline_us)
    };
    if !in_range {
        return Err(VideoError::new(
            VideoErrorCode::InvalidTimestamp,
            if endpoint {
                "timeline endpoint is outside the clip"
            } else {
                "timeline sample is outside the clip"
            },
        ));
    }
    let offset = timeline_us
        .0
        .checked_sub(clip.timeline_start_us.0)
        .ok_or_else(arithmetic_overflow)? as i128;
    let source_offset = divide_nonnegative(
        offset
            .checked_mul(clip.playback_rate.numerator as i128)
            .ok_or_else(arithmetic_overflow)?,
        clip.playback_rate.denominator as i128,
        mode,
    )?;
    let mapped = (clip.source_range.start_us.0 as i128)
        .checked_add(source_offset)
        .ok_or_else(arithmetic_overflow)?;
    let mapped = Microseconds(checked_i128_to_i64(mapped)?);
    let mapped_in_range = if endpoint {
        clip.source_range.contains_endpoint(mapped)
    } else {
        clip.source_range.contains(mapped)
    };
    if !mapped_in_range {
        return Err(VideoError::new(
            VideoErrorCode::InvalidTimestamp,
            "mapped timestamp escaped the clip source range",
        ));
    }
    Ok(mapped)
}

pub fn map_source_to_timeline(
    clip: &TimelineClip,
    source_us: Microseconds,
    mode: QuantizeMode,
) -> VideoResult<Microseconds> {
    map_source_to_timeline_inner(clip, source_us, mode, false)
}

/// Converts a source edit boundary, including a source range's exclusive end,
/// to its exact timeline boundary. Sample lookup remains half-open through
/// [`map_source_to_timeline`].
pub fn map_source_endpoint_to_timeline(
    clip: &TimelineClip,
    source_us: Microseconds,
    mode: QuantizeMode,
) -> VideoResult<Microseconds> {
    map_source_to_timeline_inner(clip, source_us, mode, true)
}

fn map_source_to_timeline_inner(
    clip: &TimelineClip,
    source_us: Microseconds,
    mode: QuantizeMode,
    endpoint: bool,
) -> VideoResult<Microseconds> {
    clip.validate()?;
    let in_range = if endpoint {
        clip.source_range.contains_endpoint(source_us)
    } else {
        clip.source_range.contains(source_us)
    };
    if !in_range {
        return Err(VideoError::new(
            VideoErrorCode::InvalidTimestamp,
            if endpoint {
                "source endpoint is outside the clip"
            } else {
                "source sample is outside the clip"
            },
        ));
    }
    let offset = source_us
        .0
        .checked_sub(clip.source_range.start_us.0)
        .ok_or_else(arithmetic_overflow)? as i128;
    let timeline_offset = divide_nonnegative(
        offset
            .checked_mul(clip.playback_rate.denominator as i128)
            .ok_or_else(arithmetic_overflow)?,
        clip.playback_rate.numerator as i128,
        mode,
    )?;
    let mapped = (clip.timeline_start_us.0 as i128)
        .checked_add(timeline_offset)
        .ok_or_else(arithmetic_overflow)?;
    let mapped = Microseconds(checked_i128_to_i64(mapped)?);
    let timeline_range = clip.timeline_range()?;
    let mapped_in_range = if endpoint {
        timeline_range.contains_endpoint(mapped)
    } else {
        timeline_range.contains(mapped)
    };
    if !mapped_in_range {
        return Err(VideoError::new(
            VideoErrorCode::InvalidTimestamp,
            "mapped timestamp escaped the clip timeline range",
        ));
    }
    Ok(mapped)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TimelineSpanKind {
    Clip { clip_id: String },
    Gap { gap_id: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimelineSpan {
    pub range: TimeRange,
    pub kind: TimelineSpanKind,
}

/// Produces the exact ordered partition used by rendering. A gap-preserving track must
/// cover `[0, timeline_duration_us)` with clips or explicit gaps and may not overlap.
pub fn partition_track(
    track: &TimelineTrack,
    gaps: &[TimelineGap],
    timeline_duration_us: Microseconds,
) -> VideoResult<Vec<TimelineSpan>> {
    if timeline_duration_us.0 <= 0 {
        return Err(VideoError::new(
            VideoErrorCode::InvalidTimestamp,
            "timeline duration must be positive",
        ));
    }
    let mut spans = Vec::with_capacity(track.clips.len() + gaps.len());
    for clip in &track.clips {
        clip.validate()?;
        spans.push(TimelineSpan {
            range: clip.timeline_range()?,
            kind: TimelineSpanKind::Clip {
                clip_id: clip.id.clone(),
            },
        });
    }
    for gap in gaps.iter().filter(|gap| gap.track_id == track.id) {
        gap.validate()?;
        spans.push(TimelineSpan {
            range: gap.range,
            kind: TimelineSpanKind::Gap {
                gap_id: gap.id.clone(),
            },
        });
    }
    spans.sort_by(|left, right| {
        (left.range.start_us, left.range.end_us).cmp(&(right.range.start_us, right.range.end_us))
    });

    let mut cursor = Microseconds::ZERO;
    for span in &spans {
        if span.range.start_us < cursor {
            return Err(VideoError::new(
                VideoErrorCode::TimelineOverlap,
                "track clips and gaps overlap",
            ));
        }
        if track.preserve_gaps && span.range.start_us > cursor {
            return Err(VideoError::new(
                VideoErrorCode::TimelineGap,
                "gap-preserving track has an implicit hole",
            ));
        }
        if span.range.end_us > timeline_duration_us {
            return Err(VideoError::new(
                VideoErrorCode::InvalidTimestamp,
                "track span exceeds timeline duration",
            ));
        }
        cursor = span.range.end_us;
    }
    if track.preserve_gaps && cursor != timeline_duration_us {
        return Err(VideoError::new(
            VideoErrorCode::TimelineGap,
            "gap-preserving track does not reach timeline end",
        ));
    }
    Ok(spans)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceClockSpanKind {
    Selected,
    Gap,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceClockSpan {
    pub range: TimeRange,
    pub kind: SourceClockSpanKind,
}

/// Turns ordered transcript/selection ranges into a complete source-clock partition.
/// Silent holes remain first-class gaps rather than being collapsed by concatenation.
pub fn source_clock_partition(
    mut selected: Vec<TimeRange>,
    source_duration_us: Microseconds,
) -> VideoResult<Vec<SourceClockSpan>> {
    if source_duration_us.0 <= 0 {
        return Err(VideoError::new(
            VideoErrorCode::InvalidTimestamp,
            "source duration must be positive",
        ));
    }
    selected.sort_by_key(|range| (range.start_us, range.end_us));
    let mut cursor = Microseconds::ZERO;
    let mut spans = Vec::with_capacity(selected.len().saturating_mul(2).saturating_add(1));
    for range in selected {
        range.validate()?;
        if range.end_us > source_duration_us {
            return Err(VideoError::new(
                VideoErrorCode::InvalidTimestamp,
                "selection exceeds source clock",
            ));
        }
        if range.start_us < cursor {
            return Err(VideoError::new(
                VideoErrorCode::TimelineOverlap,
                "source selections overlap",
            ));
        }
        if range.start_us > cursor {
            spans.push(SourceClockSpan {
                range: TimeRange::new(cursor.0, range.start_us.0)?,
                kind: SourceClockSpanKind::Gap,
            });
        }
        spans.push(SourceClockSpan {
            range,
            kind: SourceClockSpanKind::Selected,
        });
        cursor = range.end_us;
    }
    if cursor < source_duration_us {
        spans.push(SourceClockSpan {
            range: TimeRange::new(cursor.0, source_duration_us.0)?,
            kind: SourceClockSpanKind::Gap,
        });
    }
    if spans.is_empty() {
        spans.push(SourceClockSpan {
            range: TimeRange::new(0, source_duration_us.0)?,
            kind: SourceClockSpanKind::Gap,
        });
    }
    Ok(spans)
}

fn divide_nonnegative(numerator: i128, denominator: i128, mode: QuantizeMode) -> VideoResult<i128> {
    if numerator < 0 || denominator <= 0 {
        return Err(VideoError::new(
            VideoErrorCode::ArithmeticOverflow,
            "internal rational operands must be non-negative with a positive denominator",
        ));
    }
    let quotient = numerator / denominator;
    let remainder = numerator % denominator;
    match mode {
        QuantizeMode::Floor => Ok(quotient),
        QuantizeMode::Ceil => quotient
            .checked_add(i128::from(remainder != 0))
            .ok_or_else(arithmetic_overflow),
        QuantizeMode::Nearest => {
            let doubled = remainder.checked_mul(2).ok_or_else(arithmetic_overflow)?;
            quotient
                .checked_add(i128::from(doubled >= denominator))
                .ok_or_else(arithmetic_overflow)
        }
    }
}

fn checked_i128_to_i64(value: i128) -> VideoResult<i64> {
    i64::try_from(value).map_err(|_| arithmetic_overflow())
}

fn arithmetic_overflow() -> VideoError {
    VideoError::new(
        VideoErrorCode::ArithmeticOverflow,
        "timeline rational arithmetic overflowed",
    )
}

#[cfg(test)]
mod tests {
    use super::super::contracts::{
        GapReason, MediaReference, RationalRate, TimelineClip, TimelineGap, TimelineTrack,
        TrackKind,
    };
    use super::*;

    fn clip(
        id: &str,
        source_start: i64,
        source_end: i64,
        timeline_start: i64,
        timeline_duration: i64,
        rate: RationalRate,
    ) -> TimelineClip {
        TimelineClip {
            id: id.into(),
            scene_id: None,
            media: MediaReference {
                source_asset_id: Some("source-1".into()),
                render_artifact_id: None,
            },
            source_range: TimeRange::new(source_start, source_end).unwrap(),
            timeline_start_us: Microseconds(timeline_start),
            timeline_duration_us: Microseconds(timeline_duration),
            playback_rate: rate,
            gain_db_milli: 0,
            muted: false,
            crop: None,
        }
    }

    #[test]
    fn ntsc_frame_quantization_has_no_hour_scale_drift() {
        let fps = RationalFrameRate::FPS_30000_1001;
        let hour = Microseconds(3_600_000_000);
        assert_eq!(
            frame_index_at(hour, fps, QuantizeMode::Floor).unwrap(),
            107_892
        );
        assert_eq!(
            frame_time_us(107_892, fps, QuantizeMode::Nearest).unwrap(),
            Microseconds(3_599_996_400)
        );
        assert_eq!(
            frame_index_at(
                frame_time_us(107_892, fps, QuantizeMode::Nearest).unwrap(),
                fps,
                QuantizeMode::Nearest,
            )
            .unwrap(),
            107_892
        );
    }

    #[test]
    fn outward_quantization_never_trims() {
        let range = TimeRange::new(1, 41_667).unwrap();
        let quantized = quantize_range_outward(range, RationalFrameRate::FPS_24).unwrap();
        assert_eq!(quantized.start_frame, 0);
        assert_eq!(quantized.end_frame, 2);
        assert!(quantized.start_us <= range.start_us);
        assert!(quantized.end_us >= range.end_us);
    }

    #[test]
    fn rational_playback_maps_both_directions_exactly() {
        let clip = clip(
            "fast",
            2_000_000,
            4_000_000,
            10_000_000,
            1_000_000,
            RationalRate {
                numerator: 2,
                denominator: 1,
            },
        );
        let source =
            map_timeline_to_source(&clip, Microseconds(10_250_000), QuantizeMode::Nearest).unwrap();
        assert_eq!(source, Microseconds(2_500_000));
        assert_eq!(
            map_source_to_timeline(&clip, source, QuantizeMode::Nearest).unwrap(),
            Microseconds(10_250_000)
        );
        assert_eq!(
            map_timeline_to_source(&clip, Microseconds(11_000_000), QuantizeMode::Nearest,)
                .unwrap_err()
                .code,
            VideoErrorCode::InvalidTimestamp
        );
        assert_eq!(
            map_timeline_endpoint_to_source(
                &clip,
                Microseconds(11_000_000),
                QuantizeMode::Nearest,
            )
            .unwrap(),
            Microseconds(4_000_000)
        );
        assert_eq!(
            map_source_endpoint_to_timeline(&clip, Microseconds(4_000_000), QuantizeMode::Nearest,)
                .unwrap(),
            Microseconds(11_000_000)
        );
    }

    #[test]
    fn adjacent_clips_have_single_sample_owner_at_boundary() {
        let first = clip("first", 0, 1_000_000, 0, 1_000_000, RationalRate::ONE);
        let second = clip(
            "second",
            1_000_000,
            2_000_000,
            1_000_000,
            1_000_000,
            RationalRate::ONE,
        );
        let boundary = Microseconds(1_000_000);
        assert!(map_timeline_to_source(&first, boundary, QuantizeMode::Nearest).is_err());
        assert_eq!(
            map_timeline_to_source(&second, boundary, QuantizeMode::Nearest).unwrap(),
            Microseconds(1_000_000)
        );
        assert_eq!(
            map_timeline_endpoint_to_source(&first, boundary, QuantizeMode::Nearest).unwrap(),
            Microseconds(1_000_000)
        );
    }

    #[test]
    fn explicit_silence_is_part_of_track_partition() {
        let track = TimelineTrack {
            id: "main".into(),
            kind: TrackKind::Video,
            clips: vec![
                clip("a", 0, 1_000_000, 0, 1_000_000, RationalRate::ONE),
                clip(
                    "b",
                    2_000_000,
                    3_000_000,
                    2_000_000,
                    1_000_000,
                    RationalRate::ONE,
                ),
            ],
            preserve_gaps: true,
        };
        let gaps = vec![TimelineGap {
            id: "silence".into(),
            track_id: "main".into(),
            range: TimeRange::new(1_000_000, 2_000_000).unwrap(),
            reason: GapReason::SourceSilence,
            source_asset_id: Some("source-1".into()),
            source_range: Some(TimeRange::new(1_000_000, 2_000_000).unwrap()),
        }];
        let spans = partition_track(&track, &gaps, Microseconds(3_000_000)).unwrap();
        assert_eq!(spans.len(), 3);
        assert!(matches!(spans[1].kind, TimelineSpanKind::Gap { .. }));
    }

    #[test]
    fn implicit_silence_is_rejected() {
        let track = TimelineTrack {
            id: "main".into(),
            kind: TrackKind::Video,
            clips: vec![clip(
                "a",
                1_000_000,
                2_000_000,
                1_000_000,
                1_000_000,
                RationalRate::ONE,
            )],
            preserve_gaps: true,
        };
        let error = partition_track(&track, &[], Microseconds(2_000_000)).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::TimelineGap);
    }

    #[test]
    fn source_clock_partition_keeps_leading_internal_and_trailing_gaps() {
        let spans = source_clock_partition(
            vec![
                TimeRange::new(1_000_000, 2_000_000).unwrap(),
                TimeRange::new(3_000_000, 4_000_000).unwrap(),
            ],
            Microseconds(5_000_000),
        )
        .unwrap();
        assert_eq!(spans.len(), 5);
        assert_eq!(spans[0].kind, SourceClockSpanKind::Gap);
        assert_eq!(spans[1].kind, SourceClockSpanKind::Selected);
        assert_eq!(
            spans[2].range,
            TimeRange::new(2_000_000, 3_000_000).unwrap()
        );
        assert_eq!(spans[4].kind, SourceClockSpanKind::Gap);
    }
}
