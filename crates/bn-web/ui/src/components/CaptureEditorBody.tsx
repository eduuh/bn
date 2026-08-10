import "@blocknote/core/fonts/inter.css";
import "@blocknote/ariakit/style.css";

import { BlockNoteSchema, defaultBlockSpecs, filterSuggestionItems } from "@blocknote/core";
import { BlockNoteView } from "@blocknote/ariakit";
import {
  getDefaultReactSlashMenuItems,
  SuggestionMenuController,
  useCreateBlockNote,
} from "@blocknote/react";
import { useCallback, useEffect, useMemo, useRef } from "react";

import type { PendingFile } from "../lib/capture";
import { ATTACHMENT_SCHEME, fileToPending } from "../lib/capture";

/**
 * The BlockNote half of [`CaptureEditor`], loaded as its own chunk.
 *
 * BlockNote is ProseMirror plus a UI kit — several times the weight of the whole rest of the
 * dashboard's vendor bundle. Nothing above the fold needs it, and most sessions never open the
 * capture dialog at all, so it is behind the same `lazy()` boundary `MarkdownBody` already uses.
 *
 * # Markdown is the output, never the input
 *
 * The editor's own block JSON is the live document; markdown is produced from it on every change
 * and is the only thing that leaves this component. It is deliberately one-way:
 * `blocksToMarkdownLossy` is lossy by name, and the file on the other end of this is the task doc
 * the SMART gate reads `## Done When` out of. Authoring new prose through a lossy exporter costs
 * nothing; re-parsing an agent-written doc through it would silently drop acceptance criteria.
 */
export default function CaptureEditorBody({
  initialMarkdown,
  onChange,
  onFiles,
  files,
}: {
  initialMarkdown: string;
  onChange: (markdown: string) => void;
  onFiles: (files: PendingFile[]) => void;
  files: PendingFile[];
}) {
  // Video/audio blocks would let a capture carry something the notes repo has no business
  // storing; images and files are the two the task doc actually documents.
  const schema = useMemo(() => {
    const { video, audio, ...blockSpecs } = defaultBlockSpecs;
    void video;
    void audio;
    return BlockNoteSchema.create({ blockSpecs });
  }, []);

  // The pending files are read inside `uploadFile`, which BlockNote captures once. A ref keeps
  // that callback looking at the current list instead of the one from first render.
  const pending = useRef(files);
  pending.current = files;

  /**
   * Where a pasted or dropped file goes.
   *
   * There is no task id yet — the task does not exist until Create — so there is nothing to upload
   * *to*. The bytes are held in the dialog's pending list and the block is pointed at an opaque
   * `attachment:<name>` URL. On submit, `capture.ts` rewrites those to bare file names, which is
   * the identifier attachments are known by everywhere else: the doc's manifest, the API path, and
   * `bn task attach`.
   */
  const uploadFile = useCallback(
    async (file: File) => {
      const taken = new Set(pending.current.map((f) => f.name));
      const stored = await fileToPending(file, taken);
      onFiles([...pending.current, stored]);
      return `${ATTACHMENT_SCHEME}${stored.name}`;
    },
    [onFiles],
  );

  const editor = useCreateBlockNote({ schema, uploadFile }, [schema]);

  // Seed from the restored draft exactly once. Re-running this on every `initialMarkdown` change
  // would fight the operator's typing, since the markdown this emits comes back as that prop.
  const seeded = useRef(false);
  useEffect(() => {
    if (seeded.current) return;
    seeded.current = true;
    if (!initialMarkdown.trim()) return;
    const blocks = editor.tryParseMarkdownToBlocks(initialMarkdown);
    if (blocks.length) editor.replaceBlocks(editor.document, blocks);
  }, [editor, initialMarkdown]);

  const emit = useCallback(() => {
    onChange(editor.blocksToMarkdownLossy(editor.document));
  }, [editor, onChange]);

  return (
    <div className="max-h-64 overflow-y-auto rounded-md border border-border bg-background py-2">
      <BlockNoteView
        editor={editor}
        onChange={emit}
        // The dashboard owns the page theme; `theme` here only picks BlockNote's own token set.
        theme="light"
        slashMenu={false}
        data-capture-editor
      >
        {/* The default slash menu offers block types the schema no longer has. Filtering the
            React item list keeps the menu honest rather than letting it insert a broken block. */}
        <SuggestionMenuController
          triggerCharacter="/"
          getItems={(query) =>
            Promise.resolve(filterSuggestionItems(getDefaultReactSlashMenuItems(editor), query))
          }
        />
      </BlockNoteView>
    </div>
  );
}
