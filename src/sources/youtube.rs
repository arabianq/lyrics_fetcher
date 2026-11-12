use super::{LyricsSource, Track, http::Http};
use crate::lyrics::{Line, Lyrics, MAX_LINES};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde_json::{Value, json};

pub struct YoutubeSource {
    pub http: Http,
}
const SONGS: &str = "contents.tabbedSearchResultsRenderer.tabs.tabRenderer.content.sectionListRenderer.contents.musicShelfRenderer.contents.musicResponsiveListItemRenderer";
const TABS: &str = "contents.singleColumnMusicWatchNextResultsRenderer.tabbedRenderer.watchNextTabbedResultsRenderer.tabs.tabRenderer";
const ROWS: &str = "contents.elementRenderer.newElement.type.componentType.model.timedLyricsModel.lyricsData.timedLyricsData";
const COLUMNS: &str = "flexColumns.musicResponsiveListItemFlexColumnRenderer";
const BROWSE: &str = "endpoint.browseEndpoint";
fn dig<'a>(value: &'a Value, path: &str) -> Vec<&'a Value> {
    let mut values = vec![value];
    for key in path.split('.') {
        let mut next = Vec::new();
        for v in values {
            match v {
                Value::Array(items) => {
                    for item in items {
                        if let Some(v) = item.get(key) {
                            next.push(v)
                        }
                    }
                }
                _ => {
                    if let Some(v) = v.get(key) {
                        next.push(v)
                    }
                }
            }
        }
        values = next;
    }
    values
        .into_iter()
        .flat_map(|v| {
            if let Value::Array(a) = v {
                a.iter().collect()
            } else {
                vec![v]
            }
        })
        .collect()
}
fn texts(value: &Value, path: &str) -> Vec<String> {
    dig(value, path)
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}
fn text(value: &Value, path: &str) -> String {
    texts(value, path).into_iter().next().unwrap_or_default()
}
fn language(track: &Track) -> &'static str {
    let value = format!("{}{}", track.artist, track.title);
    if value.chars().any(|c| matches!(c as u32, 0xAC00..=0xD7AF)) {
        "ko"
    } else if value
        .chars()
        .any(|c| matches!(c as u32,0x3040..=0x30FF|0x3400..=0x9FFF))
    {
        "ja"
    } else {
        "en"
    }
}
fn web_version() -> String {
    // Gregorian civil date from days since Unix epoch, without a local timezone dependency.
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 86400;
    let z = days as i64 + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    y += i64::from(m <= 2);
    format!("1.{y:04}{m:02}{d:02}.01.00")
}
impl YoutubeSource {
    async fn call(&self, endpoint: &str, fields: &str, body: Value) -> Result<Option<Value>> {
        self.http
            .json(
                self.http
                    .client
                    .post(format!(
                        "https://music.youtube.com/youtubei/v1/{endpoint}?prettyPrint=false"
                    ))
                    .header("X-Goog-FieldMask", fields)
                    .json(&body),
            )
            .await
    }
}
#[async_trait]
impl LyricsSource for YoutubeSource {
    fn name(&self) -> &'static str {
        "youtubemusic"
    }
    async fn fetch(&self, track: &Track) -> Result<Option<Lyrics>> {
        let web = json!({"client":{"clientName":"WEB_REMIX","clientVersion":web_version(),"hl":language(track)}});
        let Some(search)=self.call("search",&format!("{SONGS}(playlistItemData.videoId,flexColumns.musicResponsiveListItemFlexColumnRenderer.text.runs.text)"),json!({"context":web,"query":format!("{} {}",track.title,track.artist),"params":"EgWKAQIIAWoMEA4QChADEAQQCRAF"})).await?else{return Ok(None)};
        let mut candidates = Vec::new();
        for song in dig(&search, SONGS) {
            let columns = dig(song, COLUMNS);
            if columns.len() < 2 {
                continue;
            }
            let title = texts(columns[0], "text.runs.text").join("");
            let details = texts(columns[1], "text.runs.text");
            let duration = details.last().and_then(|s| crate::lyrics::timestamp(s));
            let artists = &details[..details
                .iter()
                .position(|s| s == " • ")
                .unwrap_or(details.len())];
            let artist = artists.join("");
            let video = text(song, "playlistItemData.videoId");
            if !video.is_empty() && track.matches(&title, &artist, duration) {
                candidates.push((
                    duration.unwrap_or(0).abs_diff(track.duration),
                    video,
                    duration,
                ));
            }
        }
        candidates.sort_by_key(|c| c.0);
        let Some((_, video, duration)) = candidates.into_iter().next() else {
            return Ok(None);
        };
        let Some(next) = self
            .call(
                "next",
                &format!(
                    "{TABS}(unselectable,{BROWSE}(browseId,browseEndpointContextSupportedConfigs))"
                ),
                json!({"context":web,"videoId":video}),
            )
            .await?
        else {
            return Ok(None);
        };
        let id=dig(&next,TABS).into_iter().find(|tab|text(tab,&format!("{BROWSE}.browseEndpointContextSupportedConfigs.browseEndpointContextMusicConfig.pageType"))=="MUSIC_PAGE_TYPE_TRACK_LYRICS" && !tab.get("unselectable").and_then(Value::as_bool).unwrap_or(false)).map(|tab|text(tab,&format!("{BROWSE}.browseId"))).unwrap_or_default();
        if id.is_empty() {
            return Ok(None);
        }
        let Some(page)=self.call("browse",ROWS,json!({"context":{"client":{"clientName":"ANDROID_MUSIC","clientVersion":"7.21.50","hl":"en"}},"browseId":id})).await?else{return Ok(None)};
        let rows = dig(&page, ROWS);
        let mut lyrics = decode(&rows)?;
        lyrics.duration = duration;
        Ok((!lyrics.empty()).then_some(lyrics))
    }
}
fn decode(rows: &[&Value]) -> Result<Lyrics> {
    ensure!(rows.len() <= MAX_LINES, "too many YouTube lyrics lines");
    let plain = rows
        .iter()
        .map(|v| text(v, "lyricLine"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut lyrics = Lyrics {
        plain,
        ..Lyrics::default()
    };
    for row in rows {
        let start = text(row, "cueRange.startTimeMilliseconds").parse::<u64>();
        let end = text(row, "cueRange.endTimeMilliseconds").parse::<u64>();
        let (Ok(start), Ok(end)) = (start, end) else {
            lyrics.lines.clear();
            break;
        };
        if end < start {
            lyrics.lines.clear();
            break;
        }
        lyrics.lines.push(Line {
            start,
            end: Some(end),
            text: text(row, "lyricLine"),
            ..Line::default()
        });
    }
    lyrics.validate()?;
    Ok(lyrics)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn follows_arrays_and_falls_back_for_partial_timing() {
        let data = json!({"items":[{"runs":[{"text":"a"},{"text":"b"}]}]});
        assert_eq!(texts(&data, "items.runs.text"), ["a", "b"]);
        let a = json!({"lyricLine":"a","cueRange":{"startTimeMilliseconds":"1000","endTimeMilliseconds":"2000"}});
        let b = json!({"lyricLine":"b"});
        assert!(decode(&[&a, &b]).unwrap().lines.is_empty());
        assert_eq!(decode(&[&a]).unwrap().lines[0].start, 1000);
    }
}
