//! 工作表图片的后台解码与有界纹理缓存。

use crate::SheetApp;
use eframe::egui;
use std::collections::HashMap;
use std::path::PathBuf;

const IMG_CACHE_MAX: usize = 256;
const IMG_CACHE_VRAM_BUDGET: usize = 192 * 1024 * 1024;

enum ImgDecodeJob {
    ZipPart {
        key: String,
        wb_path: PathBuf,
        part: String,
        max_dim: u32,
        wake: egui::Context,
    },
    MemBytes {
        key: String,
        bytes: std::sync::Arc<Vec<u8>>,
        max_dim: u32,
        wake: egui::Context,
    },
}

pub(super) enum ImgTexState {
    Ready(egui::TextureHandle),
    Loading,
    Failed,
}

#[derive(Default)]
pub(super) struct ImgCache {
    entries: HashMap<String, Option<egui::TextureHandle>>,
    entry_bytes: HashMap<String, usize>,
    bytes_used: usize,
    lru: std::collections::VecDeque<String>,
    in_flight: std::collections::HashSet<String>,
    job_tx: Option<std::sync::mpsc::Sender<ImgDecodeJob>>,
    done_rx: Option<std::sync::mpsc::Receiver<(String, Option<egui::ColorImage>)>>,
}

impl ImgCache {
    #[inline]
    pub(super) fn bucket_dim(px: f32) -> u32 {
        let p = px.ceil().max(64.0) as u32;
        for b in [256u32, 512, 1024, 2048, 4096] {
            if p <= b {
                return b;
            }
        }
        4096
    }

    fn decode_color_image(bytes: &[u8], max_dim: u32) -> Option<egui::ColorImage> {
        let mut img = image::load_from_memory(bytes).ok()?;
        let (w, h) = (img.width(), img.height());
        if max_dim > 0 && (w > max_dim || h > max_dim) {
            img = img.resize(max_dim, max_dim, image::imageops::FilterType::Triangle);
        }
        let rgba = img.to_rgba8();
        let size = [rgba.width() as usize, rgba.height() as usize];
        Some(egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()))
    }

    fn ensure_worker(&mut self) -> &std::sync::mpsc::Sender<ImgDecodeJob> {
        if self.job_tx.is_none() {
            let (jtx, jrx) = std::sync::mpsc::channel::<ImgDecodeJob>();
            let (dtx, drx) = std::sync::mpsc::channel::<(String, Option<egui::ColorImage>)>();
            let shared_rx = std::sync::Arc::new(std::sync::Mutex::new(jrx));
            let workers = std::thread::available_parallelism()
                .map(|n| n.get().clamp(2, 4))
                .unwrap_or(4);
            for idx in 0..workers {
                let rx_c = shared_rx.clone();
                let dtx_c = dtx.clone();
                let _ = std::thread::Builder::new()
                    .name(format!("cellan-img-{idx}"))
                    .spawn(move || loop {
                        let job = match rx_c.lock() {
                            Ok(g) => g.recv().ok(),
                            Err(_) => None,
                        };
                        let Some(job) = job else { break };
                        let (key, img, wake) = match job {
                            ImgDecodeJob::ZipPart {
                                key,
                                wb_path,
                                part,
                                max_dim,
                                wake,
                            } => {
                                let decoded = sheetcore::pic::read_part(&wb_path, &part)
                                    .and_then(|bytes| Self::decode_color_image(&bytes, max_dim));
                                (key, decoded, wake)
                            }
                            ImgDecodeJob::MemBytes {
                                key,
                                bytes,
                                max_dim,
                                wake,
                            } => {
                                let decoded = Self::decode_color_image(&bytes, max_dim);
                                (key, decoded, wake)
                            }
                        };
                        if dtx_c.send((key, img)).is_err() {
                            break;
                        }
                        wake.request_repaint();
                    });
            }
            self.job_tx = Some(jtx);
            self.done_rx = Some(drx);
        }
        self.job_tx.as_ref().unwrap()
    }

    fn touch_lru(&mut self, key: &str) {
        if self.lru.back().map(String::as_str) == Some(key) {
            return;
        }
        if let Some(pos) = self.lru.iter().position(|k| k == key) {
            self.lru.remove(pos);
        }
        self.lru.push_back(key.to_owned());
    }

    fn insert_lru(&mut self, key: String, tex: Option<egui::TextureHandle>) {
        let new_bytes = tex
            .as_ref()
            .map(|t| {
                let sz = t.size();
                sz[0].saturating_mul(sz[1]).saturating_mul(4)
            })
            .unwrap_or(0);
        if let Some(old_b) = self.entry_bytes.remove(&key) {
            self.bytes_used = self.bytes_used.saturating_sub(old_b);
        }
        if !self.entries.contains_key(&key) {
            while self.lru.len() >= IMG_CACHE_MAX
                || (!self.lru.is_empty()
                    && self.bytes_used.saturating_add(new_bytes) > IMG_CACHE_VRAM_BUDGET)
            {
                if let Some(evict) = self.lru.pop_front() {
                    self.entries.remove(&evict);
                    if let Some(b) = self.entry_bytes.remove(&evict) {
                        self.bytes_used = self.bytes_used.saturating_sub(b);
                    }
                }
            }
            self.lru.push_back(key.clone());
        } else {
            self.touch_lru(&key);
        }
        self.bytes_used = self.bytes_used.saturating_add(new_bytes);
        self.entry_bytes.insert(key.clone(), new_bytes);
        self.entries.insert(key, tex);
    }

    pub(super) fn poll(&mut self, ctx: &egui::Context) {
        if self.in_flight.is_empty() {
            return;
        }
        let mut ready = Vec::new();
        if let Some(rx) = &self.done_rx {
            while let Ok(item) = rx.try_recv() {
                ready.push(item);
            }
        }
        for (key, img) in ready {
            self.in_flight.remove(&key);
            let tex = img.map(|color| {
                ctx.load_texture(key.clone(), color, egui::TextureOptions::LINEAR)
            });
            self.insert_lru(key, tex);
        }
    }

    fn get_or_request_part(
        &mut self,
        key: &str,
        wb_path: Option<&std::path::Path>,
        part: &str,
        max_dim: u32,
        sync_now: bool,
        ctx: &egui::Context,
    ) -> ImgTexState {
        if let Some(v) = self.entries.get(key).cloned() {
            self.touch_lru(key);
            return match v {
                Some(t) => ImgTexState::Ready(t),
                None => ImgTexState::Failed,
            };
        }
        let Some(path) = wb_path else {
            self.insert_lru(key.to_owned(), None);
            return ImgTexState::Failed;
        };
        if sync_now {
            let loaded = sheetcore::pic::read_part(path, part)
                .and_then(|bytes| Self::decode_color_image(&bytes, max_dim))
                .map(|color| ctx.load_texture(key.to_owned(), color, egui::TextureOptions::LINEAR));
            self.insert_lru(key.to_owned(), loaded.clone());
            return match loaded {
                Some(t) => ImgTexState::Ready(t),
                None => ImgTexState::Failed,
            };
        }
        if self.in_flight.insert(key.to_owned()) {
            let tx = self.ensure_worker();
            let _ = tx.send(ImgDecodeJob::ZipPart {
                key: key.to_owned(),
                wb_path: path.to_path_buf(),
                part: part.to_owned(),
                max_dim,
                wake: ctx.clone(),
            });
        }
        ImgTexState::Loading
    }

    fn get_or_request_mem(
        &mut self,
        key: &str,
        bytes: &std::sync::Arc<Vec<u8>>,
        max_dim: u32,
        sync_now: bool,
        ctx: &egui::Context,
    ) -> ImgTexState {
        if let Some(v) = self.entries.get(key).cloned() {
            self.touch_lru(key);
            return match v {
                Some(t) => ImgTexState::Ready(t),
                None => ImgTexState::Failed,
            };
        }
        if sync_now {
            let loaded = Self::decode_color_image(bytes, max_dim)
                .map(|color| ctx.load_texture(key.to_owned(), color, egui::TextureOptions::LINEAR));
            self.insert_lru(key.to_owned(), loaded.clone());
            return match loaded {
                Some(t) => ImgTexState::Ready(t),
                None => ImgTexState::Failed,
            };
        }
        if self.in_flight.insert(key.to_owned()) {
            let tx = self.ensure_worker();
            let _ = tx.send(ImgDecodeJob::MemBytes {
                key: key.to_owned(),
                bytes: bytes.clone(),
                max_dim,
                wake: ctx.clone(),
            });
        }
        ImgTexState::Loading
    }
}

impl SheetApp {
    /// 获取或后台异步解码 ZIP 图片纹理（多线程按需降采样 + 192MB 显存预算 LRU）
    pub(super) fn img_texture_state(
        &mut self,
        key: &str,
        part: &str,
        max_dim: u32,
        sync_now: bool,
        ctx: &egui::Context,
    ) -> ImgTexState {
        let path = self.wb.path.clone();
        self.img_cache
            .get_or_request_part(key, path.as_deref(), part, max_dim, sync_now, ctx)
    }

    /// 内存字节的图片纹理（新插入、尚未保存；多线程按需降采样 + 显存预算 LRU）
    pub(super) fn mem_texture_state(
        &mut self,
        key: &str,
        bytes: &std::sync::Arc<Vec<u8>>,
        max_dim: u32,
        sync_now: bool,
        ctx: &egui::Context,
    ) -> ImgTexState {
        self.img_cache
            .get_or_request_mem(key, bytes, max_dim, sync_now, ctx)
    }
}

#[cfg(test)]
mod img_cache_tests {
    use super::{IMG_CACHE_MAX, ImgCache};

    #[test]
    fn img_cache_evicts_lru_over_bound() {
        let mut cache = ImgCache::default();
        for i in 0..(IMG_CACHE_MAX + 12) {
            cache.insert_lru(format!("k{i}"), None);
        }
        assert_eq!(cache.entries.len(), IMG_CACHE_MAX);
        assert_eq!(cache.lru.len(), IMG_CACHE_MAX);
        // Earliest 12 keys were evicted; newest key remains.
        assert!(!cache.entries.contains_key("k0"));
        assert!(!cache.entries.contains_key("k11"));
        assert!(cache.entries.contains_key("k12"));
        assert!(cache.entries.contains_key(&format!("k{}", IMG_CACHE_MAX + 11)));

        // Touching k12 moves it to MRU so next insert evicts k13 instead of k12.
        cache.touch_lru("k12");
        cache.insert_lru("k_extra".into(), None);
        assert_eq!(cache.entries.len(), IMG_CACHE_MAX);
        assert!(cache.entries.contains_key("k12"));
        assert!(!cache.entries.contains_key("k13"));
        assert!(cache.entries.contains_key("k_extra"));
    }
}
