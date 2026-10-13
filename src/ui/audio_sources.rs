//! Assign a player once without combining independently controllable streams.
use crate::{media, volume};
use std::{cmp::Reverse, collections::HashMap};

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
