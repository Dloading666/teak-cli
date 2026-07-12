// Shared task model — consumed by both the to-do list view (TaskBoard) and
// the sticky-note view (TaskNoteView). Kept in its own module so the two
// views import the same types/constants without a circular runtime import
// (TaskBoard imports the TaskNoteView component; both import from here).

export type TaskStatus = 'todo' | 'working' | 'done';

export interface TaskItem {
  id: string;
  title: string;
  description?: string;
  status: TaskStatus;
  createdAt: number;
  // Sticky-note view only: the user-dragged body height in px (bottom-edge
  // resize). Undefined = auto-grow to fit content from a roomy default. Ignored
  // by the to-do list view. Persisted so a note keeps the size you gave it.
  height?: number;
}

// Builds the welcome/guide note. Shared by the first-launch seed (TaskBoard)
// and the empty-state "show guide" button, so both produce an identical roomy
// note. Caller passes the already-localized guide text.
export function makeWelcomeNote(text: string): TaskItem {
  return {
    id: crypto.randomUUID(),
    title: text,
    status: 'todo',
    createdAt: Date.now(),
    height: 240,
  };
}

// Click-to-advance order for the to-do checkbox (todo → working → done → todo).
export const NEXT_STATUS: Record<TaskStatus, TaskStatus> = {
  todo: 'working',
  working: 'done',
  done: 'todo',
};

// Vertical grouping order shared by both views: 进行中 (top) → 待办 → 已完成.
// The sticky-note view renders this same order with no section headers — the
// per-card status dots carry the grouping signal instead.
export const STATUS_ORDER: TaskStatus[] = ['working', 'todo', 'done'];

// Builds a blank prompt-library entry for the "prompt" view mode. In that
// mode `title` is repurposed as the CATEGORY name (same-title items group
// together via 100% string match) and `description` holds the prompt body.
// A freshly created category is just one of these with an empty body - it
// keeps the category alive until the user writes a body or deletes it; when
// the last item sharing a title is removed, the category vanishes naturally
// (no separate "delete category" affordance). `status` is unused by this
// view but kept populated so the same TaskItem reads correctly in list/note.
export function makePromptItem(category: string): TaskItem {
  return {
    id: crypto.randomUUID(),
    title: category,
    description: '',
    status: 'todo',
    createdAt: Date.now(),
  };
}
