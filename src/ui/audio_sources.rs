//! Assign a player once without combining independently controllable streams.
use crate::{media, volume};
use std::{cmp::Reverse, collections::HashMap};

const STREAM_GRACE: std::time::Duration = std::time::Duration::from_millis(900);

#[derive(Clone)]
pub struct Row {
    pub stream: volume::Stream,
    pub key: u64,
    missing_since: Option<std::time::Instant>,
}
impl Row {
    pub fn live(&self) -> bool {
        self.missing_since.is_none()
    }
}
#[derive(Default)]
pub struct Rows(pub Vec<Row>, u64);
impl Rows {
    pub fn update(
        &mut self,
        streams: &[volume::Stream],
        now: std::time::Instant,
    ) -> Option<std::time::Duration> {
        // Browsers can retain a suspended stream after recreating playback for
        // the same media. Keep its active counterpart, but preserve unrelated
        // paused sources and independently playing streams.
        let superseded = |stream: &volume::Stream| {
            stream.corked
                && stream.pid != 0
                && stream
                    .media
                    .as_ref()
                    .is_some_and(|title| !title.trim().is_empty())
                && streams.iter().any(|other| {
                    !other.corked
                        && other.pid == stream.pid
                        && other.application == stream.application
                        && other.media == stream.media
                })
        };
        self.0.retain(|row| {
            !superseded(
                streams
                    .iter()
                    .find(|stream| stream.index == row.stream.index)
                    .unwrap_or(&row.stream),
            )
        });
        let mut remaining: Vec<_> = streams
            .iter()
            .filter(|stream| !superseded(stream))
            .cloned()
            .collect();
        // Match surviving server IDs before considering recreated streams.
        for row in &mut self.0 {
            if let Some(index) = remaining.iter().position(|s| s.index == row.stream.index) {
                row.stream = remaining.remove(index);
                row.missing_since = None;
            } else {
                row.missing_since.get_or_insert(now);
            }
        }
        let mut returning = HashMap::new();
        for stream in &remaining {
            *returning
                .entry((stream.pid, stream.application.clone()))
                .or_insert(0) += 1;
        }
        for stream in remaining {
            // Only replace an unambiguous source from the same application process.
            let matches: Vec<_> = self
                .0
                .iter()
                .enumerate()
                .filter(|(_, row)| {
                    row.missing_since.is_some()
                        && stream.pid != 0
                        && row.stream.pid == stream.pid
                        && row.stream.application == stream.application
                })
                .map(|(i, _)| i)
                .collect();
            if matches.len() == 1 && returning[&(stream.pid, stream.application.clone())] == 1 {
                let row = &mut self.0[matches[0]];
                row.stream = stream;
                row.missing_since = None;
            } else {
                self.0.push(Row {
                    key: self.1,
                    stream,
                    missing_since: None,
                });
                self.1 += 1;
            }
        }
        self.0.retain(|row| {
            row.missing_since
                .is_none_or(|since| now.duration_since(since) < STREAM_GRACE)
        });
        self.0
            .iter()
            .filter_map(|row| {
                row.missing_since
                    .map(|since| STREAM_GRACE.saturating_sub(now.duration_since(since)))
            })
            .min()
    }
}

pub fn player_rows<'a>(
    streams: &[volume::Stream],
    tracks: &'a [media::Track],
) -> HashMap<u32, &'a media::Track> {
    let mut assigned = HashMap::new();
    for track in tracks {
        let title = track.title.to_lowercase();
        if let Some(stream) = streams
            .iter()
            .filter(|stream| {
                !assigned.contains_key(&stream.index)
                    && media::player_matches(&stream.application, &track.player)
            })
            .max_by_key(|stream| {
                (
                    !title.is_empty() && stream.name.to_lowercase().contains(&title),
                    stream.corked != track.playing,
                    Reverse(stream.index),
                )
            })
        {
            assigned.insert(stream.index, track);
        }
    }
    assigned
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(index: u32, pid: u32) -> volume::Stream {
        volume::Stream {
            index,
            pid,
            media: None,
            application: "Music".into(),
            icon: "music".into(),
            name: "Track".into(),
            percent: 75,
            muted: false,
            corked: false,
        }
    }
    #[test]
    fn suspended_browser_duplicate_yields_to_active_stream() {
        let now = std::time::Instant::now();
        let mut rows = Rows::default();
        let mut old = fixture(1, 100);
        old.media = Some("Video".into());
        rows.update(&[old.clone()], now);
        old.corked = true;
        let mut active = old.clone();
        active.index = 2;
        active.corked = false;
        active.percent = 85;
        rows.update(&[old.clone(), active.clone()], now);
        assert_eq!(rows.0.len(), 1);
        assert_eq!(rows.0[0].stream.index, 2);
        assert_eq!(rows.0[0].stream.percent, 85);

        // Pausing the remaining source must retain its slider and row.
        let key = rows.0[0].key;
        active.corked = true;
        rows.update(&[active.clone()], now);
        assert_eq!(rows.0.len(), 1);
        assert_eq!(rows.0[0].key, key);

        // A different title, process, unknown title, or two active streams
        // cannot be treated as a suspended duplicate.
        for variant in 0..4 {
            let mut other = old.clone();
            active.corked = false;
            match variant {
                0 => other.media = Some("Other video".into()),
                1 => other.pid = 200,
                2 => other.media = None,
                _ => other.corked = false,
            }
            let mut separate = Rows::default();
            separate.update(&[other, active.clone()], now);
            assert_eq!(separate.0.len(), 2);
        }
    }
    #[test]
    fn transient_stream_removal_and_recreation_keep_the_row_and_order() {
        let now = std::time::Instant::now();
        let mut rows = Rows::default();
        rows.update(&[fixture(1, 100), fixture(2, 200)], now);
        rows.update(&[fixture(2, 200)], now);
        assert_eq!(rows.0.len(), 2);
        assert!(!rows.0[0].live());
        let mut resumed = fixture(3, 100);
        resumed.corked = true;
        resumed.name = "New track".into();
        rows.update(
            &[fixture(2, 200), resumed],
            now + std::time::Duration::from_millis(400),
        );
        assert_eq!(
            rows.0.iter().map(|r| r.stream.index).collect::<Vec<_>>(),
            [3, 2]
        );
        assert_eq!(rows.0[0].key, 0);
        assert!(rows.0[0].live());
        assert!(rows.0[0].stream.corked);
        assert_eq!(rows.0[0].stream.name, "New track");
        rows.update(&[], now + std::time::Duration::from_secs(1));
        assert_eq!(rows.0.len(), 2);
        rows.update(&[], now + std::time::Duration::from_secs(2));
        assert!(rows.0.is_empty());
    }
    #[test]
    fn one_recreated_stream_keeps_its_row_among_other_streams_from_the_same_process() {
        let now = std::time::Instant::now();
        let mut rows = Rows::default();
        rows.update(&[fixture(1, 100), fixture(2, 100)], now);
        rows.update(&[fixture(2, 100)], now);
        rows.update(&[fixture(2, 100), fixture(3, 100)], now);
        assert_eq!(
            rows.0.iter().map(|r| r.stream.index).collect::<Vec<_>>(),
            [3, 2]
        );
        assert!(rows.0.iter().all(Row::live));
    }
    #[test]
    fn independent_streams_from_one_process_are_never_combined() {
        let now = std::time::Instant::now();
        let mut rows = Rows::default();
        rows.update(&[fixture(1, 100), fixture(2, 100)], now);
        rows.update(&[fixture(3, 100), fixture(4, 100)], now);
        assert_eq!(rows.0.iter().filter(|r| r.live()).count(), 2);
        assert_eq!(rows.0.len(), 4);
        rows.update(
            &[fixture(3, 100), fixture(4, 100)],
            now + std::time::Duration::from_secs(1),
        );
        assert_eq!(rows.0.len(), 2);
    }
    #[test]
    fn browser_track_appears_once_and_follows_the_matching_stream() {
        let stream = |index, corked, name: &str| volume::Stream {
            pid: 0,
            media: None,
            index,
            corked,
            name: name.into(),
            application: "Firefox".into(),
            icon: "firefox".into(),
            percent: 100,
            muted: false,
        };
        let streams = vec![stream(1, true, "Firefox"), stream(2, false, "Firefox")];
        let tracks = vec![media::Track {
            player: "org.mpris.MediaPlayer2.firefox".into(),
            title: "Video".into(),
            artist: String::new(),
            art_url: String::new(),
            playing: true,
            can_previous: false,
            can_next: false,
            can_toggle: true,
        }];
        let assigned = player_rows(&streams, &tracks);
        assert_eq!(assigned.len(), 1);
        assert!(assigned.contains_key(&2));
        let streams = vec![
            stream(1, true, "Firefox · Video"),
            stream(2, false, "Firefox"),
        ];
        assert!(player_rows(&streams, &tracks).contains_key(&1));
    }
}
