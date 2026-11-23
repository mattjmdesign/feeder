use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;

use feeder_core::{FeedUpdate, MediaKind, media_kind_for_path};
use rune::{Image, ImageCache, Player};

use crate::files::ProjectView;
use crate::{State, Tab, feed};

const IMAGE_SIZE_PT: f32 = 960.0;
const ROW_OVERSCAN: usize = 3;
const INITIAL_WINDOW: Range<usize> = 0..8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaSlide {
    pub id: i64,
    pub path: PathBuf,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl MediaSlide {
    pub fn kind(&self) -> Option<MediaKind> {
        media_kind_for_path(&self.path)
    }

    pub fn is_image(&self) -> bool {
        self.kind() == Some(MediaKind::Image)
    }
}

pub fn media_slides(u: &FeedUpdate) -> Vec<MediaSlide> {
    if !u.media.is_empty() {
        u.media.iter().map(|m| MediaSlide { id: m.id, path: m.path.clone(), width: m.width, height: m.height }).collect()
    } else if let Some(path) = &u.screenshot {
        vec![MediaSlide { id: -u.id, path: path.clone(), width: u.media_w, height: u.media_h }]
    } else {
        Vec::new()
    }
}

pub fn sync(s: &mut State) {
    if s.images.is_none() {
        return;
    }
    let slides = desired_slides(s);
    let Some(cache) = s.images.as_ref() else { return };

    let mut live = HashSet::with_capacity(slides.len());
    for slide in &slides {
        live.insert(slide.id);
        match slide.kind() {
            Some(MediaKind::Video) => {
                s.videos.entry(slide.id).or_insert_with(|| {
                    let player = Player::file(&slide.path);
                    player.set_looping(true);
                    player.preload();
                    if let Some(w) = &s.wake {
                        player.set_waker(w.clone());
                    }
                    player
                });
            }
            Some(MediaKind::Image) | None => {
                s.shots.entry(slide.id).or_insert_with(|| decode_image(cache, &slide.path));
            }
        }
    }
    if !(slides.is_empty() && on_other_view(s)) {
        s.shots.retain(|id, _| live.contains(id));
    }
    s.videos.retain(|id, _| live.contains(id));
}

fn on_other_view(s: &State) -> bool {
    let project_updates = s.open.is_some() && s.files.view == ProjectView::Updates;
    let feed = s.open.is_none() && s.tab == Tab::Feed;
    !project_updates && !feed && s.lightbox.is_none()
}

fn decode_image(cache: &ImageCache, path: &std::path::Path) -> Image {
    let img = cache.file(path, IMAGE_SIZE_PT, 2.0);
    if rune::tooling::headless_run().is_some() {
        let _ = img.wait();
    }
    img
}

fn row_window(visible: Option<&Range<usize>>, total: usize) -> Range<usize> {
    if total == 0 {
        return 0..0;
    }
    match visible {
        Some(range) if !range.is_empty() => {
            let start = range.start.saturating_sub(ROW_OVERSCAN).min(total);
            let end = range.end.saturating_add(ROW_OVERSCAN).min(total);
            start..end
        }
        _ => INITIAL_WINDOW.start..INITIAL_WINDOW.end.min(total),
    }
}

fn desired_slides(s: &State) -> Vec<MediaSlide> {
    let mut needed: HashMap<i64, MediaSlide> = HashMap::new();
    let mut add = |slide: MediaSlide| {
        needed.entry(slide.id).or_insert(slide);
    };

    if let Some(open) = s.open {
        if s.files.view == ProjectView::Updates && s.feed_for == Some(open) {
            let visible = feed::visible_feed(s);
            let range = row_window(s.project_visible_rows.as_ref(), visible.len());
            for u in &visible[range] {
                collect_update_slides(s, u, false, &mut add);
            }
        }
    } else if s.tab == Tab::Feed {
        let visible = s.filtered_timeline();
        let range = row_window(s.feed_visible_rows.as_ref(), visible.len());
        for (u, _) in &visible[range] {
            collect_update_slides(s, u, false, &mut add);
        }
    }

    if let Some(id) = s.lightbox
        && let Some((_, _, slides)) = s.find_media(id)
    {
        for slide in slides {
            add(slide);
        }
    }
    if let Some(compare_id) = s.lightbox_compare
        && let Some((_, idx, slides)) = s.find_media(compare_id)
        && let Some(compare) = slides.get(idx)
    {
        add(compare.clone());
    }

    needed.into_values().collect()
}

fn collect_update_slides(s: &State, u: &FeedUpdate, all_slides: bool, add: &mut impl FnMut(MediaSlide)) {
    let slides = media_slides(u);
    if slides.is_empty() {
        return;
    }
    if all_slides {
        for slide in slides {
            add(slide);
        }
    } else {
        let idx = s.card_slide_index(u.id, slides.len());
        add(slides.into_iter().nth(idx).expect("card_slide_index is bounded by slides.len()"));
    }
}

#[cfg(test)]
mod tests {
    use feeder_core::{MediaAttachment, Orb, Project, ProjectSummary};

    use super::*;

    fn sample_summary(id: i64) -> ProjectSummary {
        ProjectSummary {
            project: Project {
                id,
                slug: format!("p{id}"),
                name: format!("Project {id}"),
                path: None,
                orb: Orb::generate(&[]),
                created_at: 1,
                archived: false,
                pinned: false,
                brief: None,
                seen_at: None,
            },
            updates: 1,
            last_update_id: Some(5),
            last_at: Some(10),
            last_agent: Some("Claude Code".into()),
            last_via: None,
            last_body: Some("Done".into()),
            needs_you: false,
            unread_replies: 0,
            active_pulse: None,
            open_todos: 0,
        }
    }

    fn sample_carousel_update(id: i64, project_id: i64) -> FeedUpdate {
        FeedUpdate {
            id,
            project_id,
            agent: "Claude Code".into(),
            branch: None,
            body: "Done: carousel update.".into(),
            screenshot: None,
            media_w: None,
            media_h: None,
            media: vec![
                MediaAttachment {
                    id: id * 10 + 1,
                    update_id: id,
                    position: 0,
                    path: PathBuf::from("/tmp/a.png"),
                    width: Some(800),
                    height: Some(600),
                    caption: None,
                },
                MediaAttachment {
                    id: id * 10 + 2,
                    update_id: id,
                    position: 1,
                    path: PathBuf::from("/tmp/b.png"),
                    width: Some(800),
                    height: Some(600),
                    caption: None,
                },
            ],
            diff_stat: None,
            commit_sha: None,
            attention_dismissed_at: None,
            via: None,
            created_at: 10,
        }
    }

    #[test]
    fn screenshots_are_kept_only_off_the_media_views() {
        let mut s = State::new();
        s.tab = Tab::Home;
        assert!(on_other_view(&s));
        s.tab = Tab::Notebook;
        assert!(on_other_view(&s));
        s.tab = Tab::Feed;
        assert!(!on_other_view(&s));
        s.tab = Tab::Home;
        s.lightbox = Some(1);
        assert!(!on_other_view(&s));
    }

    #[test]
    fn desired_slides_scopes_to_visible_rows_active_slide_and_lightbox() {
        let mut s = State::new();
        s.projects.push(sample_summary(1));
        for id in 1..=15 {
            s.timeline.push(sample_carousel_update(id, 1));
        }

        s.tab = Tab::Home;
        assert!(desired_slides(&s).is_empty());
        s.tab = Tab::Notebook;
        assert!(desired_slides(&s).is_empty());

        s.tab = Tab::Feed;
        s.feed_visible_rows = Some(0..2);
        let mut ids: Vec<i64> = desired_slides(&s).into_iter().map(|m| m.id).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![11, 21, 31, 41, 51]);

        s.set_card_slide(1, 1);
        let mut ids: Vec<i64> = desired_slides(&s).into_iter().map(|m| m.id).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![12, 21, 31, 41, 51]);

        s.open = Some(1);
        s.feed_for = Some(1);
        s.feed.push(sample_carousel_update(5, 1));
        let mut media_ids: Vec<i64> = desired_slides(&s).into_iter().map(|m| m.id).collect();
        media_ids.sort_unstable();
        assert_eq!(media_ids, vec![51]);

        s.files.view = ProjectView::Files;
        assert!(desired_slides(&s).is_empty());

        s.lightbox = Some(51);
        let mut lb_ids: Vec<i64> = desired_slides(&s).into_iter().map(|m| m.id).collect();
        lb_ids.sort_unstable();
        assert_eq!(lb_ids, vec![51, 52]);
    }
}
