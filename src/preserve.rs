//! The §11 preservation instruction, and the prompt wrapper that carries it.
//!
//! SPEC.md §11 is the AI editing contract. `eval/FINDINGS.md` measured what
//! honouring it is worth: a naive "clean this up" rewrite keeps ~5% of markers,
//! the same rewrite carrying this instruction keeps ~96-100%, across five models
//! and three vendors. That is a ~20x effect and it outweighs model tier, which
//! makes the instruction the lever and the post-edit check (`lint --before`) the
//! backstop.
//!
//! [`PRESERVE_INSTRUCTION`] is the normative phrasing of §11's six obligations
//! (preserve / keep-attached / mint-new / never-reuse / report-dropped /
//! report-duplicate), worded for a model rather than for a tool author, and
//! folding in the exact wording the eval measured rather than a paraphrase of it.
//!
//! The text is held byte-identical across the Python, JavaScript, and Rust
//! implementations by the shared conformance corpus
//! (`conformance/spec/preserve.json`, category `preserve`): each implementation
//! carries its own copy so an installed package needs no corpus on disk, and each
//! implementation's own test suite fails if its copy drifts from the corpus.

use alloc::string::String;

use crate::text::ascii_trim;

/// The SPEC.md §11 preservation instruction, worded for an editing agent.
pub const PRESERVE_INSTRUCTION: &str = r#"This Markdown document uses markstay markers: HTML comments of the form
`<!-- stay:ID ... -->` (or, in MDX, `{/* stay:ID ... */}`) placed on or just after
the block they identify. Each marker is a stable address that other tools rely on,
so it must survive your edit.

When you edit this document you MUST:

- preserve every existing `stay:` marker exactly as written, including its id and
  any `hash=` / `quote=` attributes; do not remove, reword, renumber, or relocate it;
- keep each marker attached to the same logical block it was on before, even when
  you move, reword, or reformat that block;
- mint a fresh id (any new short token) only for content that is genuinely new;
- never reuse an existing id for different content;
- if you must drop a marker, report it explicitly in your reply, never drop one
  silently;
- never introduce a duplicate id (the same id on two blocks).

Return the edited Markdown with every original marker still present and in place."#;

/// The return-format line the eval's measured prompt shape appends.
pub const PRESERVE_RETURN_ONLY: &str =
    "Return only the resulting Markdown, with no commentary and no code fence around it.";

/// Compose a ready-to-send editing prompt: optional task, the preservation
/// instruction, the return-format line, then the document behind a `---` rule.
///
/// This is the prompt shape `eval/run_eval.py` measured, so a caller reproduces
/// the measured survival rate rather than an approximation of it. A task that is
/// empty or only ASCII whitespace is treated as absent.
///
/// Trimming uses the crate's ASCII whitespace set (§8/§9), not [`str::trim`], so
/// three implementations compose byte-identical prompts: `trim` would also eat
/// NBSP and every other Unicode `White_Space`, which are document content.
pub fn preserve_wrap(doc: &str, task: Option<&str>) -> String {
    let mut out = String::new();
    if let Some(t) = task {
        let trimmed = ascii_trim(t);
        if !trimmed.is_empty() {
            out.push_str(trimmed);
            out.push_str("\n\n");
        }
    }
    out.push_str(PRESERVE_INSTRUCTION);
    out.push_str("\n\n");
    out.push_str(PRESERVE_RETURN_ONLY);
    out.push_str("\n\n---\n\n");
    out.push_str(ascii_trim(doc));
    out.push('\n');
    out
}
