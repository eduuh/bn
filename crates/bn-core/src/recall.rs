//! Semantic recall over the whole note corpus — the decision log that finally pays interest.
//!
//! `bn search` (see [`crate::search`]) is a fast, dumb substring scan. `bn recall` is the
//! quality tool: it chunks every note (active *and* closed) plus the sibling investigation docs
//! into section-sized units, embeds each into a vector, and ranks a query by cosine similarity
//! blended with recency and an exact-substring bonus — so recall strictly dominates search while
//! also surfacing work phrased differently.
//!
//! Design constraints from issue #249:
//!   - **Local-first, offline**: the default [`Embedder`] is a pure-Rust hashed token/n-gram
//!     term-frequency vectorizer — no model download, no network, no native deps. It's kept
//!     behind the [`Embedder`] trait so a neural/API embedder can be dropped in via config later
//!     without touching the index, ranker, or CLI.
//!   - **Machine-local, derivable index**: the store lives under the OS cache dir (see
//!     [`default_index_path`]), *never* the synced notes repo — it's rebuildable per machine.
//!   - **Incremental**: the index is keyed by file fingerprint (mtime + size); `--index`
//!     re-embeds only changed files and reuses vectors for the rest, so a full rebuild over a
//!     real corpus completes in seconds.
//!   - **Degrades loudly**: a cold/absent/stale index makes `recall` fall back to `search`
//!     semantics with a notice; the auto-surface path only ever *reads* a warm index and skips
//!     silently when cold, so note creation never blocks on embedding.

use crate::resolver::Config;
use crate::search::{self, SearchHit};
use crate::store;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Longest chunk text kept (chars). Long doc sections are truncated so the index stays small and
/// embedding cost is bounded; recall matches on the head of a section, which carries its topic.
const CHUNK_TEXT_MAX: usize = 1200;

/// Default embedding dimensionality for [`HashEmbedder`]. 512 buckets keeps hash collisions rare
/// for a note-sized vocabulary while leaving the per-chunk vector small (2 KiB of `f32`).
const DEFAULT_DIM: usize = 512;

/// Recency half-life (days) for the ranking blend: a chunk this old contributes half its recency
/// weight. Tuned long — durable decisions stay findable for months, which is the whole point.
const HALF_LIFE_DAYS: f64 = 120.0;

/// Ranking blend weights. Cosine leads; the substring bonus guarantees recall ⊇ search quality
/// (anything a substring scan would find is boosted to the top); recency is a gentle tiebreaker.
const W_COSINE: f32 = 1.0;
const W_SUBSTRING: f32 = 0.6;
const W_RECENCY: f32 = 0.2;

/// One indexed unit of a note — a section-sized slice with the metadata a recall hit reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chunk {
    pub repo: String,
    pub branch: String,
    /// `goal` | `decision` | `research` | `todo` | `blocker` | `collab` | `ask` | `section`
    /// (a `##` body section) | `doc` (a heading section of a sibling investigation file).
    pub kind: String,
    /// The chunk's date (`YYYY-MM-DD`): the item's own date when it has one, else the owning
    /// note's `created`. Empty when unknown (e.g. a search-fallback hit). Drives recency.
    pub date: String,
    /// The owning note's status (`active`/`closed`) so closed work is clearly labelled.
    pub status: String,
    pub path: String,
    /// 1-based line where the chunk starts; `0` for frontmatter-derived chunks (no stable line).
    pub line: usize,
    pub text: String,
}

/// A ranked recall result: the matched chunk and its blended score.
#[derive(Debug, Clone, Serialize)]
pub struct RecallHit {
    #[serde(flatten)]
    pub chunk: Chunk,
    pub score: f32,
}

/// Where a [`RecallOutcome`]'s hits came from — the index proper, or the `search` fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RecallSource {
    Index,
    Fallback,
}

/// The result of a `recall` query: ranked hits plus whether the index was used or the query
/// degraded to substring search (with a human-readable reason to print).
#[derive(Debug, Clone)]
pub struct RecallOutcome {
    pub hits: Vec<RecallHit>,
    pub source: RecallSource,
    /// Present iff `source == Fallback`: why the index couldn't be used.
    pub degraded_reason: Option<String>,
}

// ---------------------------------------------------------------------------------------------
// Embedder trait + the pure-Rust default
// ---------------------------------------------------------------------------------------------

/// Turns text into fixed-width vectors for cosine similarity. The corpus is embedded once at
/// index time and queries at query time, so a change of embedder must invalidate the index —
/// callers compare [`Embedder::id`] against the stored id and rebuild on mismatch.
pub trait Embedder {
    /// Embed a batch of texts, returning one vector per input (same order).
    fn embed(&self, texts: &[String]) -> Vec<Vec<f32>>;
    /// Vector width. Stored in the index; a mismatch forces a rebuild.
    fn dim(&self) -> usize;
    /// Stable identity of this embedder (scheme + dimension). Stored in the index; a mismatch
    /// forces a rebuild so a smarter embedder's vectors never get compared against old ones.
    fn id(&self) -> &str;
}

/// The offline default: a hashed token/n-gram term-frequency vectorizer. Pure Rust, deterministic,
/// no model, no network. Each text contributes its tokens (unigrams), adjacent token bigrams, and
/// per-token character trigrams into `dim` hashed buckets, L2-normalized so cosine is a plain dot
/// product. Character trigrams give morphological robustness ("debounce"/"debouncing" overlap),
/// so recall generalizes past exact substrings — the edge over `bn search`. A neural/API embedder
/// can replace this behind the [`Embedder`] trait later without touching anything else.
#[derive(Debug, Clone)]
pub struct HashEmbedder {
    dim: usize,
    id: String,
}

impl Default for HashEmbedder {
    fn default() -> Self {
        Self::new(DEFAULT_DIM)
    }
}

impl HashEmbedder {
    pub fn new(dim: usize) -> Self {
        let dim = dim.max(1);
        Self {
            dim,
            id: format!("hash-tfngram-v1-d{dim}"),
        }
    }
}

impl Embedder for HashEmbedder {
    fn embed(&self, texts: &[String]) -> Vec<Vec<f32>> {
        texts.iter().map(|t| embed_one(t, self.dim)).collect()
    }
    fn dim(&self) -> usize {
        self.dim
    }
    fn id(&self) -> &str {
        &self.id
    }
}

/// The default embedder used by the CLI (and the sensible starting point for tests).
pub fn default_embedder() -> HashEmbedder {
    HashEmbedder::default()
}

/// Embed one text into a `dim`-wide L2-normalized bag of hashed features.
fn embed_one(text: &str, dim: usize) -> Vec<f32> {
    let mut v = vec![0f32; dim];
    let toks = tokenize(text);
    for (i, tok) in toks.iter().enumerate() {
        bump(&mut v, dim, b"t:", tok, 1.0);
        for tri in char_trigrams(tok) {
            bump(&mut v, dim, b"g:", &tri, 0.3);
        }
        if let Some(next) = toks.get(i + 1) {
            bump(&mut v, dim, b"b:", &format!("{tok}_{next}"), 0.7);
        }
    }
    l2_normalize(&mut v);
    v
}

/// Add `weight` to the bucket for `prefix + key` (prefixing keeps token/bigram/trigram spaces
/// from colliding by construction).
fn bump(v: &mut [f32], dim: usize, prefix: &[u8], key: &str, weight: f32) {
    let mut h = fnv1a(prefix);
    h = fnv1a_add(h, key.as_bytes());
    v[(h % dim as u64) as usize] += weight;
}

/// Lowercased alphanumeric runs of length ≥ 2 (single chars and punctuation carry no signal).
fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            cur.extend(ch.to_lowercase());
        } else if !cur.is_empty() {
            if cur.chars().count() >= 2 {
                out.push(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
        }
    }
    if cur.chars().count() >= 2 {
        out.push(cur);
    }
    out
}

/// Sliding character trigrams of a token; the whole token when shorter than 3 chars.
fn char_trigrams(tok: &str) -> Vec<String> {
    let chars: Vec<char> = tok.chars().collect();
    if chars.len() < 3 {
        return vec![tok.to_string()];
    }
    chars.windows(3).map(|w| w.iter().collect()).collect()
}

fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x00000100000001B3;

fn fnv1a(bytes: &[u8]) -> u64 {
    fnv1a_add(FNV_OFFSET, bytes)
}

fn fnv1a_add(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// Cosine similarity, guarding zero-norm vectors (returns 0). Vectors from [`HashEmbedder`] are
/// already normalized, but full cosine keeps the ranker correct for any embedder or fixture.
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0f32;
    let mut na = 0f32;
    let mut nb = 0f32;
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

// ---------------------------------------------------------------------------------------------
// Index store (bincode, machine-local, incremental by file fingerprint)
// ---------------------------------------------------------------------------------------------

/// Bumped whenever the on-disk layout changes; a mismatch (or any deserialize error) makes
/// [`load_index`] return `None`, so the next `--index` rebuilds from scratch.
const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    chunk: Chunk,
    vector: Vec<f32>,
}

/// One file's fingerprint + its embedded chunks. Reused wholesale when the fingerprint matches,
/// so unchanged files are never re-read or re-embedded.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileEntry {
    mtime_secs: u64,
    mtime_nanos: u32,
    size: u64,
    entries: Vec<Entry>,
}

impl FileEntry {
    fn matches(&self, fp: &Fingerprint) -> bool {
        self.mtime_secs == fp.secs && self.mtime_nanos == fp.nanos && self.size == fp.size
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Index {
    version: u32,
    embedder_id: String,
    dim: usize,
    /// Keyed by absolute file path.
    files: BTreeMap<String, FileEntry>,
}

impl Index {
    /// Whether this index can serve queries for `embedder`: right format, right embedder, and
    /// non-empty. A mismatch means the stored vectors aren't comparable to fresh query vectors.
    fn is_usable_for(&self, embedder: &dyn Embedder) -> bool {
        self.version == FORMAT_VERSION
            && self.embedder_id == embedder.id()
            && self.dim == embedder.dim()
            && !self.files.is_empty()
    }
}

struct Fingerprint {
    secs: u64,
    nanos: u32,
    size: u64,
}

fn fingerprint(path: &Path) -> Option<Fingerprint> {
    let md = std::fs::metadata(path).ok()?;
    let modified = md.modified().ok()?;
    let dur = modified
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    Some(Fingerprint {
        secs: dur.as_secs(),
        nanos: dur.subsec_nanos(),
        size: md.len(),
    })
}

/// Read the index at `path`. `None` on any absence/format/deserialize error — the caller then
/// rebuilds (for `--index`) or degrades to search (for a query), never crashes.
fn load_index(path: &Path) -> Option<Index> {
    let bytes = std::fs::read(path).ok()?;
    let index: Index = bincode::deserialize(&bytes).ok()?;
    if index.version != FORMAT_VERSION {
        return None;
    }
    Some(index)
}

fn save_index(path: &Path, index: &Index) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = bincode::serialize(index).map_err(|e| std::io::Error::other(e.to_string()))?;
    std::fs::write(path, bytes)
}

/// The machine-local index path: `$BN_RECALL_INDEX` if set, else `<cache>/bn/recall/index.bin`
/// (`~/.cache` on Linux, `~/Library/Caches` on macOS, `%LOCALAPPDATA%` on Windows). Deliberately
/// *outside* the synced notes repo — the index is derivable and per-machine (issue #249).
pub fn default_index_path() -> PathBuf {
    if let Some(p) = std::env::var_os("BN_RECALL_INDEX") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let base = dirs::cache_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("bn").join("recall").join("index.bin")
}

/// What a `--index` run did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct BuildStats {
    /// Files in the resulting index.
    pub files: usize,
    /// Chunks (embedded units) in the resulting index.
    pub chunks: usize,
    /// Chunks freshly embedded this run (changed/new files).
    pub embedded: usize,
    /// Chunks reused from the previous index (unchanged files).
    pub reused: usize,
    /// True if the whole index was rebuilt (first run or embedder changed).
    pub full_rebuild: bool,
}

/// (Re)build the index at `index_path`, embedding only files whose fingerprint changed and
/// reusing vectors for the rest. A changed embedder id/dim (or a first run) triggers a full
/// rebuild. Returns per-run [`BuildStats`]. Best-effort persistence: a write failure is swallowed
/// (the caller can inspect the returned stats), matching bn's degrade-don't-crash contract.
pub fn build_index(cfg: &Config, index_path: &Path, embedder: &dyn Embedder) -> BuildStats {
    let old = load_index(index_path);
    let full_rebuild = !old.as_ref().is_some_and(|i| {
        i.embedder_id == embedder.id() && i.dim == embedder.dim() && i.version == FORMAT_VERSION
    });
    let old_files = if full_rebuild {
        BTreeMap::new()
    } else {
        old.map(|i| i.files).unwrap_or_default()
    };

    let mut new_files: BTreeMap<String, FileEntry> = BTreeMap::new();
    let mut stats = BuildStats {
        full_rebuild,
        ..Default::default()
    };

    for root in cfg.each_notes_root() {
        let mut note_mds = Vec::new();
        find_notes(root, &mut note_mds);
        note_mds.sort();
        for note_md in note_mds {
            let mut files = vec![note_md.clone()];
            if let Some(dir) = note_md.parent() {
                files.extend(sibling_docs(dir));
            }
            let fps: Vec<Option<Fingerprint>> = files.iter().map(|f| fingerprint(f)).collect();

            // Fast path: every file in this dir is unchanged → reuse without reading anything.
            let all_reusable =
                files
                    .iter()
                    .zip(&fps)
                    .all(|(f, fp)| match (fp, old_reused(&old_files, f)) {
                        (Some(fp), Some(fe)) => fe.matches(fp),
                        _ => false,
                    });
            if all_reusable {
                for f in &files {
                    if let Some(fe) = old_files.get(&key(f)) {
                        stats.reused += fe.entries.len();
                        new_files.insert(key(f), fe.clone());
                    }
                }
                continue;
            }

            // Something changed in this dir → read the note once for chunking + doc attribution.
            let Ok(note) = store::read(&note_md) else {
                continue;
            };
            for (f, fp) in files.iter().zip(&fps) {
                let Some(fp) = fp else { continue };
                let k = key(f);
                if let Some(fe) = old_files.get(&k) {
                    if fe.matches(fp) {
                        stats.reused += fe.entries.len();
                        new_files.insert(k, fe.clone());
                        continue;
                    }
                }
                let chunks = if f == &note_md {
                    chunk_note(&note, f)
                } else {
                    chunk_doc(
                        f,
                        &note.front.repo,
                        &note.front.branch,
                        &note.front.status,
                        &note.front.created,
                    )
                };
                let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
                let vectors = embedder.embed(&texts);
                let entries: Vec<Entry> = chunks
                    .into_iter()
                    .zip(vectors)
                    .map(|(chunk, vector)| Entry { chunk, vector })
                    .collect();
                stats.embedded += entries.len();
                new_files.insert(
                    k,
                    FileEntry {
                        mtime_secs: fp.secs,
                        mtime_nanos: fp.nanos,
                        size: fp.size,
                        entries,
                    },
                );
            }
        }
    }

    stats.files = new_files.len();
    stats.chunks = new_files.values().map(|fe| fe.entries.len()).sum();
    let index = Index {
        version: FORMAT_VERSION,
        embedder_id: embedder.id().to_string(),
        dim: embedder.dim(),
        files: new_files,
    };
    crate::util::best_effort(save_index(index_path, &index), "recall: save index");
    stats
}

/// Helper so the fast-path closure reads cleanly.
fn old_reused<'a>(old: &'a BTreeMap<String, FileEntry>, path: &Path) -> Option<&'a FileEntry> {
    old.get(&key(path))
}

fn key(path: &Path) -> String {
    path.display().to_string()
}

// ---------------------------------------------------------------------------------------------
// Query + auto-surface
// ---------------------------------------------------------------------------------------------

/// Rank `query` against the index at `index_path`. When the index is warm this returns index
/// hits; when it's cold/absent/stale it degrades to [`crate::search`] semantics with a reason.
/// `repo` optionally scopes to one repo (matched on the trailing path segment, so `bn` matches
/// `owner/bn`). `today` (`YYYY-MM-DD`) anchors the recency term; `limit` caps hits (0 = default).
pub fn recall(
    cfg: &Config,
    index_path: &Path,
    query: &str,
    repo: Option<&str>,
    limit: usize,
    today: &str,
    embedder: &dyn Embedder,
) -> RecallOutcome {
    let query = query.trim();
    let limit = if limit == 0 { 20 } else { limit };
    if query.is_empty() {
        return RecallOutcome {
            hits: Vec::new(),
            source: RecallSource::Index,
            degraded_reason: None,
        };
    }

    let index = load_index(index_path);
    let warm = index.as_ref().is_some_and(|i| i.is_usable_for(embedder));
    if !warm {
        return degrade_to_search(cfg, query, repo, limit, index_path);
    }
    let Some(index) = index else {
        return degrade_to_search(cfg, query, repo, limit, index_path);
    };
    let qv = embed_query(query, embedder);
    let mut hits = rank(&index, &qv, query, repo, today);
    hits.truncate(limit);
    RecallOutcome {
        hits,
        source: RecallSource::Index,
        degraded_reason: None,
    }
}

/// Auto-surface: up to `n` warm-index hits for `query` (a fresh note's branch/goal), excluding
/// the note's own `repo`/`branch`. **Only reads** — returns empty silently when the index is
/// cold/absent/stale, so note creation never blocks on or triggers embedding. Weak matches are
/// filtered by a small score floor so we surface only genuinely-similar past work.
pub fn recall_peek(
    index_path: &Path,
    query: &str,
    exclude_repo: &str,
    exclude_branch: &str,
    n: usize,
    today: &str,
    embedder: &dyn Embedder,
) -> Vec<RecallHit> {
    let query = query.trim();
    if query.is_empty() || n == 0 {
        return Vec::new();
    }
    let Some(index) = load_index(index_path) else {
        return Vec::new();
    };
    if !index.is_usable_for(embedder) {
        return Vec::new();
    }
    let qv = embed_query(query, embedder);
    let mut hits = rank(&index, &qv, query, None, today);
    hits.retain(|h| {
        !(h.chunk.repo == exclude_repo && h.chunk.branch == exclude_branch) && h.score >= PEEK_FLOOR
    });
    hits.truncate(n);
    hits
}

/// Minimum blended score for the auto-surface path — keeps note creation from printing noise.
const PEEK_FLOOR: f32 = 0.25;

fn embed_query(query: &str, embedder: &dyn Embedder) -> Vec<f32> {
    embedder
        .embed(&[query.to_string()])
        .into_iter()
        .next()
        .unwrap_or_default()
}

/// Score every chunk in `index` against the query vector/text and return them sorted best-first.
/// Zero-scoring chunks are dropped. `repo` optionally scopes to one repo.
fn rank(index: &Index, qv: &[f32], query: &str, repo: Option<&str>, today: &str) -> Vec<RecallHit> {
    let mut hits: Vec<RecallHit> = index
        .files
        .values()
        .flat_map(|fe| fe.entries.iter())
        .filter(|e| repo.is_none_or(|r| repo_matches(&e.chunk.repo, r)))
        .map(|e| RecallHit {
            score: score(qv, query, e, today),
            chunk: e.chunk.clone(),
        })
        .filter(|h| h.score > 0.0)
        .collect();
    // Sort by score desc; tie-break newer-first then by path for determinism.
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.chunk.date.cmp(&a.chunk.date))
            .then_with(|| a.chunk.path.cmp(&b.chunk.path))
    });
    hits
}

/// The ranking blend: cosine + a substring bonus (recall ⊇ search) + a recency tiebreak.
fn score(qv: &[f32], query: &str, e: &Entry, today: &str) -> f32 {
    let cos = cosine(qv, &e.vector).max(0.0);
    let sub = substring_bonus(query, &e.chunk.text);
    let rec = recency(&e.chunk.date, today);
    W_COSINE * cos + W_SUBSTRING * sub + W_RECENCY * rec
}

/// 1.0 when the full query is a substring of the chunk (exactly what `bn search` would find),
/// otherwise the fraction of query tokens present — so lexical overlap always earns some boost.
fn substring_bonus(query: &str, text: &str) -> f32 {
    let q = query.to_lowercase();
    let t = text.to_lowercase();
    if t.contains(&q) {
        return 1.0;
    }
    let qtoks = tokenize(&q);
    if qtoks.is_empty() {
        return 0.0;
    }
    let matched = qtoks.iter().filter(|tok| t.contains(tok.as_str())).count();
    matched as f32 / qtoks.len() as f32
}

/// Exponential recency decay in `[0, 1]` with a [`HALF_LIFE_DAYS`] half-life. Unparseable/empty
/// dates and future dates return a neutral value so recency never penalizes, only rewards.
fn recency(date: &str, today: &str) -> f32 {
    let (Some(d), Some(t)) = (parse_date(date), parse_date(today)) else {
        return 0.0;
    };
    let age_days = (t - d).num_days();
    if age_days <= 0 {
        return 1.0;
    }
    0.5f64.powf(age_days as f64 / HALF_LIFE_DAYS) as f32
}

fn parse_date(s: &str) -> Option<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()
}

/// Match a chunk's repo against a `--repo` filter: case-insensitive, on the trailing path segment
/// so a gh `owner/repo` and a bare bn repo name classify the same (mirrors `Config::is_work_repo`).
fn repo_matches(chunk_repo: &str, filter: &str) -> bool {
    let base = |s: &str| s.rsplit('/').next().unwrap_or(s).to_ascii_lowercase();
    chunk_repo.eq_ignore_ascii_case(filter) || base(chunk_repo) == base(filter)
}

/// Degrade to substring search, mapping each [`SearchHit`] to a zero-scored [`RecallHit`] (search
/// order is preserved). The reason string tells the user how to warm the index.
fn degrade_to_search(
    cfg: &Config,
    query: &str,
    repo: Option<&str>,
    limit: usize,
    index_path: &Path,
) -> RecallOutcome {
    let reason = if index_path.exists() {
        "recall index is stale — showing substring matches; run `bn recall --index` to rebuild"
    } else {
        "recall index is cold — showing substring matches; run `bn recall --index` to build it"
    };
    let hits = search::search(cfg, query, 0)
        .into_iter()
        .filter(|h| repo.is_none_or(|r| repo_matches(&h.repo, r)))
        .take(limit)
        .map(hit_from_search)
        .collect();
    RecallOutcome {
        hits,
        source: RecallSource::Fallback,
        degraded_reason: Some(reason.to_string()),
    }
}

fn hit_from_search(h: SearchHit) -> RecallHit {
    RecallHit {
        chunk: Chunk {
            repo: h.repo,
            branch: h.branch,
            kind: h.kind,
            date: String::new(),
            status: h.status,
            path: h.path,
            line: h.line,
            text: h.snippet,
        },
        score: 0.0,
    }
}

// ---------------------------------------------------------------------------------------------
// Chunking
// ---------------------------------------------------------------------------------------------

/// Chunk the whole corpus (all note roots). Exposed mainly for tests; the index build inlines an
/// incremental version. Order: notes in path order, each note's chunks then its sibling docs.
pub fn chunk_corpus(cfg: &Config) -> Vec<Chunk> {
    let mut out = Vec::new();
    for root in cfg.each_notes_root() {
        let mut note_mds = Vec::new();
        find_notes(root, &mut note_mds);
        note_mds.sort();
        for note_md in note_mds {
            let Ok(note) = store::read(&note_md) else {
                continue;
            };
            out.extend(chunk_note(&note, &note_md));
            if let Some(dir) = note_md.parent() {
                for doc in sibling_docs(dir) {
                    out.extend(chunk_doc(
                        &doc,
                        &note.front.repo,
                        &note.front.branch,
                        &note.front.status,
                        &note.front.created,
                    ));
                }
            }
        }
    }
    out
}

/// Chunk a single note: its Goal, each structured item (decision/research/todo/blocker/collab/
/// ask) with the item's own date, and each non-Goal `##` body section.
fn chunk_note(note: &crate::model::Note, note_md: &Path) -> Vec<Chunk> {
    use crate::render;
    let f = &note.front;
    let path = key(note_md);
    let mut out = Vec::new();
    let mut push = |kind: &str, date: &str, line: usize, text: String| {
        let text = truncate(text.trim());
        if !text.is_empty() {
            out.push(Chunk {
                repo: f.repo.clone(),
                branch: f.branch.clone(),
                kind: kind.to_string(),
                date: if date.is_empty() {
                    f.created.clone()
                } else {
                    date.to_string()
                },
                status: f.status.clone(),
                path: path.clone(),
                line,
                text,
            });
        }
    };

    if let Some(goal) = render::goal_text(&note.body) {
        push("goal", &f.created, 0, goal);
    }
    for (kind, items) in [
        ("decision", &f.decisions),
        ("research", &f.research),
        ("todo", &f.todos),
        ("blocker", &f.blockers),
        ("collab", &f.collab),
        ("ask", &f.ask),
    ] {
        for it in items {
            push(kind, &it.created, 0, it.text.clone());
        }
    }
    for (heading, line, content) in body_sections(&note.body) {
        if heading == "Goal" {
            continue;
        }
        push("section", &f.created, line, format!("{heading}\n{content}"));
    }
    out
}

/// Chunk a sibling investigation/design doc by its markdown headings (any `#` level). A file with
/// no headings becomes a single chunk. Attribution comes from the owning note; the date is the
/// file's mtime (fallback: the note's created date).
fn chunk_doc(
    path: &Path,
    repo: &str,
    branch: &str,
    status: &str,
    note_created: &str,
) -> Vec<Chunk> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let date = file_date(path).unwrap_or_else(|| note_created.to_string());
    let pathstr = key(path);
    let mut out = Vec::new();
    let mut push = |line: usize, block: &[String]| {
        let joined = truncate(block.join("\n").trim());
        if !joined.is_empty() {
            out.push(Chunk {
                repo: repo.to_string(),
                branch: branch.to_string(),
                kind: "doc".to_string(),
                date: date.clone(),
                status: status.to_string(),
                path: pathstr.clone(),
                line,
                text: joined,
            });
        }
    };

    let mut start_line = 1usize;
    let mut block: Vec<String> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.starts_with('#') && !block.is_empty() {
            push(start_line, &block);
            block.clear();
            start_line = i + 1;
        } else if line.starts_with('#') {
            start_line = i + 1;
        }
        block.push(line.to_string());
    }
    push(start_line, &block);
    out
}

/// The note body's `##` sections as `(heading_text, start_line, content)`; content is verbatim.
fn body_sections(body: &str) -> Vec<(String, usize, String)> {
    let mut out = Vec::new();
    let mut cur: Option<(String, usize, Vec<String>)> = None;
    for (i, line) in body.lines().enumerate() {
        if let Some(h) = line.strip_prefix("## ") {
            if let Some((head, ln, content)) = cur.take() {
                out.push((head, ln, content.join("\n")));
            }
            cur = Some((h.trim().to_string(), i + 1, Vec::new()));
        } else if let Some((_, _, content)) = cur.as_mut() {
            content.push(line.to_string());
        }
    }
    if let Some((head, ln, content)) = cur.take() {
        out.push((head, ln, content.join("\n")));
    }
    out
}

/// A file's mtime as a `YYYY-MM-DD` string (UTC), for dating docs. `None` if unavailable.
fn file_date(path: &Path) -> Option<String> {
    let md = std::fs::metadata(path).ok()?;
    let modified = md.modified().ok()?;
    let dur = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    let dt = chrono::DateTime::from_timestamp(dur.as_secs() as i64, 0)?;
    Some(dt.format("%Y-%m-%d").to_string())
}

fn truncate(s: &str) -> String {
    if s.chars().count() <= CHUNK_TEXT_MAX {
        return s.to_string();
    }
    s.chars().take(CHUNK_TEXT_MAX).collect()
}

/// Recursively collect every `note.md` under `dir`, skipping VCS/dependency dirs. (Mirrors the
/// walker in [`crate::search`]; kept local so the two modules stay decoupled.)
fn find_notes(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if matches!(name, ".git" | "node_modules") {
                continue;
            }
            find_notes(&path, out);
        } else if path.file_name().and_then(|s| s.to_str()) == Some("note.md") {
            out.push(path);
        }
    }
}

/// The `*.md` design/investigation docs in `dir`, excluding the note and its achievements log.
fn sibling_docs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut docs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().and_then(|s| s.to_str()) == Some("md")
                && !matches!(
                    p.file_name().and_then(|s| s.to_str()),
                    Some("note.md") | Some("achievements.md")
                )
        })
        .collect();
    docs.sort();
    docs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::config as cfg;

    fn write_note(root: &Path, sub: &str, repo: &str, branch: &str, status: &str, body: &str) {
        write_note_dated(root, sub, repo, branch, status, "2026-06-20", body);
    }

    fn write_note_dated(
        root: &Path,
        sub: &str,
        repo: &str,
        branch: &str,
        status: &str,
        created: &str,
        body: &str,
    ) {
        let dir = root.join(sub);
        std::fs::create_dir_all(&dir).unwrap();
        let front = format!(
            "---\nrepo: {repo}\nbranch: {branch}\ncreated: {created}\nstatus: {status}\nmachine: testhost\ntags: []\ncollaborators: []\ntodos: []\nblockers: []\ndecisions: []\nresearch: []\ncollab: []\nask: []\n---\n{body}\n"
        );
        std::fs::write(dir.join("note.md"), front).unwrap();
    }

    // ---- chunker ----

    #[test]
    fn chunks_goal_items_sections_and_docs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let note = "---\nrepo: bn\nbranch: feat-x\ncreated: 2026-06-20\nstatus: active\nmachine: testhost\ntags: []\ncollaborators: []\ntodos: []\nblockers: []\ndecisions:\n- {id: d1, text: chose launchd for macOS crons, created: 2026-06-21}\nresearch:\n- {id: r1, text: investigated debounce vs rate-limit, created: 2026-06-22}\ncollab: []\nask: []\n---\n## Goal\n\nShip cron scheduling\n\n## Progress\n\nWired the timer backend\n";
        let d = root.join("bn/feat-x");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("note.md"), note).unwrap();
        std::fs::write(
            d.join("investigation.md"),
            "# Cron backends\n\nlaunchd vs systemd tradeoffs\n\n# Fallback\n\ncron table\n",
        )
        .unwrap();

        let chunks = chunk_corpus(&cfg(root));
        let kinds: Vec<&str> = chunks.iter().map(|c| c.kind.as_str()).collect();
        assert!(kinds.contains(&"goal"), "{kinds:?}");
        assert!(kinds.contains(&"decision"), "{kinds:?}");
        assert!(kinds.contains(&"research"), "{kinds:?}");
        assert!(kinds.contains(&"section"), "{kinds:?}");
        assert_eq!(
            kinds.iter().filter(|k| **k == "doc").count(),
            2,
            "{kinds:?}"
        );
        // Goal is not double-emitted as a section.
        assert_eq!(kinds.iter().filter(|k| **k == "goal").count(), 1);
        // Item date wins over note created.
        let decision = chunks.iter().find(|c| c.kind == "decision").unwrap();
        assert_eq!(decision.date, "2026-06-21");
        assert!(decision.text.contains("launchd"));
    }

    #[test]
    fn chunk_doc_without_headings_is_one_chunk() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("plain.md");
        std::fs::write(&p, "just some prose\nover two lines\n").unwrap();
        let chunks = chunk_doc(&p, "bn", "b", "active", "2026-01-01");
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].text.contains("just some prose"));
    }

    // ---- embedder ----

    #[test]
    fn embedder_is_deterministic_and_normalized() {
        let e = HashEmbedder::new(64);
        let a = &e.embed(&["cron scheduling on macOS".to_string()])[0];
        let b = &e.embed(&["cron scheduling on macOS".to_string()])[0];
        assert_eq!(a, b, "deterministic");
        let norm: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "L2-normalized, got {norm}");
    }

    #[test]
    fn related_text_scores_higher_than_unrelated() {
        let e = HashEmbedder::new(256);
        let q = &e.embed(&["how did I schedule cron jobs".to_string()])[0];
        let near = &e.embed(&["scheduled the cron job runner".to_string()])[0];
        let far = &e.embed(&["refactored the color palette css".to_string()])[0];
        assert!(
            cosine(q, near) > cosine(q, far),
            "near {} vs far {}",
            cosine(q, near),
            cosine(q, far)
        );
    }

    // ---- ranking blend (fixture embeddings) ----

    fn entry(kind: &str, date: &str, text: &str, vector: Vec<f32>) -> Entry {
        Entry {
            chunk: Chunk {
                repo: "bn".into(),
                branch: "b".into(),
                kind: kind.into(),
                date: date.into(),
                status: "active".into(),
                path: format!("/n/{kind}/note.md"),
                line: 0,
                text: text.into(),
            },
            vector,
        }
    }

    fn index_of(entries: Vec<Entry>) -> Index {
        let mut files = BTreeMap::new();
        for (i, e) in entries.into_iter().enumerate() {
            files.insert(
                format!("f{i}"),
                FileEntry {
                    mtime_secs: 0,
                    mtime_nanos: 0,
                    size: 0,
                    entries: vec![e],
                },
            );
        }
        Index {
            version: FORMAT_VERSION,
            embedder_id: "fixture".into(),
            dim: 2,
            files,
        }
    }

    #[test]
    fn ranking_blends_cosine_recency_and_substring() {
        let today = "2026-06-30";
        // Query points along dim 0.
        let qv = vec![1.0, 0.0];
        // A: higher raw cosine (0.9), but old and with no substring match.
        let a = entry(
            "decision",
            "2020-01-01",
            "unrelated words",
            vec![0.9, 0.4359],
        );
        // B: lower cosine (0.7), but contains the exact query substring (search-findable) + recent.
        let b = entry(
            "decision",
            "2026-06-29",
            "contains debounce exactly",
            vec![0.7, 0.7141],
        );
        let idx = index_of(vec![a, b]);
        let ranked = rank(&idx, &qv, "debounce", None, today);
        assert_eq!(ranked.len(), 2);
        // The substring + recency bonus lifts B above the higher-cosine A — recall ⊇ search.
        assert!(ranked[0].chunk.text.contains("debounce"), "{ranked:#?}");
        // A (the higher raw-cosine but old, non-substring chunk) is demoted to second.
        assert_eq!(ranked[1].chunk.text, "unrelated words");
    }

    #[test]
    fn recency_decays_and_future_is_neutral() {
        assert!((recency("2026-06-30", "2026-06-30") - 1.0).abs() < 1e-6);
        assert!((recency("2027-01-01", "2026-06-30") - 1.0).abs() < 1e-6); // future → neutral
        let recent = recency("2026-06-01", "2026-06-30");
        let old = recency("2025-06-30", "2026-06-30");
        assert!(recent > old, "recent {recent} old {old}");
        assert_eq!(recency("", "2026-06-30"), 0.0); // unparseable → no boost
    }

    #[test]
    fn substring_bonus_full_and_partial() {
        assert_eq!(
            substring_bonus("cron macos", "scheduling cron on macos"),
            1.0
        );
        let partial = substring_bonus("cron windows", "scheduling cron on macos");
        assert!((partial - 0.5).abs() < 1e-6, "one of two tokens: {partial}");
        assert_eq!(substring_bonus("nothing here", "totally different"), 0.0);
    }

    // ---- index build: incremental invalidation ----

    #[test]
    fn build_is_incremental_reuses_unchanged_reembeds_changed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_note(root, "bn/a", "bn", "a", "active", "## Goal\n\nalpha work\n");
        write_note(root, "bn/b", "bn", "b", "closed", "## Goal\n\nbeta work\n");
        let idxdir = tempfile::tempdir().unwrap();
        let idx = idxdir.path().join("index.bin");
        let e = HashEmbedder::new(64);

        let first = build_index(&cfg(root), &idx, &e);
        assert!(first.full_rebuild);
        assert!(first.embedded >= 2, "{first:?}");
        assert_eq!(first.reused, 0);

        // No changes → everything reused, nothing embedded.
        let second = build_index(&cfg(root), &idx, &e);
        assert!(!second.full_rebuild);
        assert_eq!(second.embedded, 0, "{second:?}");
        assert_eq!(second.reused, first.chunks);

        // Change one note → only its file re-embeds; the other is reused.
        std::thread::sleep(std::time::Duration::from_millis(10));
        write_note(
            root,
            "bn/a",
            "bn",
            "a",
            "active",
            "## Goal\n\nalpha work rewritten with more detail\n",
        );
        let third = build_index(&cfg(root), &idx, &e);
        assert!(!third.full_rebuild);
        assert!(third.embedded >= 1, "changed file re-embedded: {third:?}");
        assert!(third.reused >= 1, "unchanged file reused: {third:?}");
    }

    #[test]
    fn changed_embedder_forces_full_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_note(root, "bn/a", "bn", "a", "active", "## Goal\n\nalpha\n");
        let idxdir = tempfile::tempdir().unwrap();
        let idx = idxdir.path().join("index.bin");

        build_index(&cfg(root), &idx, &HashEmbedder::new(64));
        // Different dim → different id → full rebuild, no reuse.
        let stats = build_index(&cfg(root), &idx, &HashEmbedder::new(128));
        assert!(stats.full_rebuild);
        assert_eq!(stats.reused, 0);
    }

    // ---- query + degrade ----

    #[test]
    fn recall_warm_returns_ranked_index_hits_including_closed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_note(
            root,
            "archive/bn/old",
            "bn",
            "old",
            "closed",
            "## Goal\n\nhandle cron scheduling on macOS with launchd\n",
        );
        write_note(
            root,
            "bn/other",
            "bn",
            "other",
            "active",
            "## Goal\n\nrefactor the css color palette\n",
        );
        let idxdir = tempfile::tempdir().unwrap();
        let idx = idxdir.path().join("index.bin");
        let e = HashEmbedder::new(256);
        build_index(&cfg(root), &idx, &e);

        let out = recall(
            &cfg(root),
            &idx,
            "cron on macos",
            None,
            10,
            "2026-06-30",
            &e,
        );
        assert_eq!(out.source, RecallSource::Index);
        assert!(out.degraded_reason.is_none());
        assert!(!out.hits.is_empty());
        let top = &out.hits[0];
        assert_eq!(top.chunk.branch, "old");
        assert_eq!(top.chunk.status, "closed", "recall reaches closed notes");
    }

    #[test]
    fn recall_cold_degrades_to_search_with_notice() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_note(
            root,
            "bn/a",
            "bn",
            "a",
            "active",
            "## Goal\n\ntelemetry markers\n",
        );
        let idx = dir.path().join("does-not-exist.bin");
        let e = HashEmbedder::new(64);
        let out = recall(&cfg(root), &idx, "telemetry", None, 10, "2026-06-30", &e);
        assert_eq!(out.source, RecallSource::Fallback);
        assert!(out.degraded_reason.unwrap().contains("cold"));
        assert!(out.hits.iter().any(|h| h.chunk.text.contains("telemetry")));
    }

    #[test]
    fn repo_filter_scopes_results() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_note(
            root,
            "bn/a",
            "bn",
            "a",
            "active",
            "## Goal\n\nshared word here\n",
        );
        write_note(
            root,
            "other/a",
            "other",
            "a",
            "active",
            "## Goal\n\nshared word here\n",
        );
        let idxdir = tempfile::tempdir().unwrap();
        let idx = idxdir.path().join("index.bin");
        let e = HashEmbedder::new(128);
        build_index(&cfg(root), &idx, &e);

        let out = recall(
            &cfg(root),
            &idx,
            "shared word",
            Some("bn"),
            10,
            "2026-06-30",
            &e,
        );
        assert!(!out.hits.is_empty());
        assert!(
            out.hits.iter().all(|h| h.chunk.repo == "bn"),
            "{:#?}",
            out.hits
        );
    }

    #[test]
    fn peek_is_silent_when_cold_and_excludes_self() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_note(
            root,
            "bn/self",
            "bn",
            "self",
            "active",
            "## Goal\n\ncron scheduling launchd\n",
        );
        write_note(
            root,
            "archive/bn/prior",
            "bn",
            "prior",
            "closed",
            "## Goal\n\ncron scheduling launchd backend\n",
        );
        let idxdir = tempfile::tempdir().unwrap();
        let idx = idxdir.path().join("index.bin");
        let e = HashEmbedder::new(256);

        // Cold → silent.
        assert!(recall_peek(&idx, "cron scheduling", "bn", "self", 3, "2026-06-30", &e).is_empty());

        build_index(&cfg(root), &idx, &e);
        let hits = recall_peek(
            &idx,
            "cron scheduling launchd",
            "bn",
            "self",
            3,
            "2026-06-30",
            &e,
        );
        assert!(!hits.is_empty(), "warm index surfaces prior work");
        assert!(
            hits.iter().all(|h| h.chunk.branch != "self"),
            "excludes the current branch: {hits:#?}"
        );
    }

    #[test]
    fn default_index_path_honors_env() {
        let _g = crate::test_support::env_lock();
        let idx = crate::test_support::EnvGuard::set("BN_RECALL_INDEX", "/tmp/custom-recall.bin");
        assert_eq!(
            default_index_path(),
            PathBuf::from("/tmp/custom-recall.bin")
        );
        drop(idx); // restore prior (absent) → exercise the cache-dir fallback below
                   // Falls back to a path under the cache dir ending in the expected suffix.
        let p = default_index_path();
        assert!(p.ends_with("bn/recall/index.bin"), "{}", p.display());
    }
}
