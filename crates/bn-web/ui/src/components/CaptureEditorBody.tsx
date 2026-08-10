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

  /**
   * The other half of `uploadFile`: turning `attachment:<name>` back into something an `<img>` can
   * actually load.
   *
   * `attachment:` is not a scheme any browser knows, so the image block that a pasted screenshot
   * produces rendered as a broken image — the file was captured correctly and stored correctly,
   * and the operator could not see the thing they had just pasted. BlockNote's `resolveFileUrl`
   * exists for exactly this ("implementing custom protocols / schemes"): the document keeps the
   * stable name, and display resolves it against the bytes we are already holding.
   */
  const resolveFileUrl = useCallback((url: string) => {
    if (!url.startsWith(ATTACHMENT_SCHEME)) return Promise.resolve(url);
    const name = url.slice(ATTACHMENT_SCHEME.length);
    // The bytes are already in memory, so this resolves immediately — the promise is only here
    // because the hook is shaped for the authenticated-URL case, where it would be a fetch.
    return Promise.resolve(pending.current.find((f) => f.name === name)?.dataUrl ?? url);
  }, []);

  const editor = useCreateBlockNote({ schema, uploadFile, resolveFileUrl }, [schema]);

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
        // The dashboard is dark-only (`tailwind.css` puts the dark palette straight in `:root`),
        // so the light token set rendered a white sheet of paper in the middle of a dark dialog.
        // `dark` is only half the fix: BlockNote's own dark grey is not this app's warm charcoal,
        // so `tailwind.css` remaps the `--bn-colors-*` tokens onto the design system's.
        theme="dark"
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
