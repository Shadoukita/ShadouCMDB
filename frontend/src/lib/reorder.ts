import { ref } from "vue";

/** Returns a copy of `list` with the item at `from` moved to index `to`. */
export function moveItem<T>(list: readonly T[], from: number, to: number): T[] {
  const out = [...list];
  if (from < 0 || from >= out.length || to < 0 || to >= out.length || from === to) return out;
  const [item] = out.splice(from, 1);
  out.splice(to, 0, item);
  return out;
}

/**
 * Drag-and-drop reordering of table rows (native HTML5 drag events). Dropping a
 * row on another moves it to that row's position; `onDrop(dragId, targetId)`
 * decides what that means. Pair it with move-up/down buttons: dragging is not
 * keyboard accessible.
 *
 *   <tr v-bind="dnd.row(item.id)">…<td class="drag-handle" aria-hidden="true">⠿</td>…</tr>
 */
export function useDragReorder(onDrop: (dragId: string, targetId: string) => void, enabled: () => boolean = () => true) {
  const dragId = ref<string | null>(null);
  const overId = ref<string | null>(null);

  function reset() {
    dragId.value = null;
    overId.value = null;
  }

  function row(id: string) {
    if (!enabled()) return {};
    return {
      draggable: true,
      class: { dragging: dragId.value === id, "drop-target": overId.value === id && dragId.value !== id },
      onDragstart: (e: DragEvent) => {
        dragId.value = id;
        if (e.dataTransfer) {
          e.dataTransfer.effectAllowed = "move";
          e.dataTransfer.setData("text/plain", id);
        }
      },
      onDragover: (e: DragEvent) => {
        if (!dragId.value) return;
        e.preventDefault();
        if (e.dataTransfer) e.dataTransfer.dropEffect = "move";
        overId.value = id;
      },
      onDragleave: () => {
        if (overId.value === id) overId.value = null;
      },
      onDrop: (e: DragEvent) => {
        e.preventDefault();
        const from = dragId.value;
        reset();
        if (from && from !== id) onDrop(from, id);
      },
      onDragend: reset,
    };
  }

  return { row, dragId, overId };
}
