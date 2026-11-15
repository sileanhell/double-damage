/**
 * Draggable handle straddling a side panel's inner border; takes no layout width.
 * `side` is the edge of the window the panel is docked to.
 */
export function PanelResizer(props: {
  side: "left" | "right";
  width: () => number;
  setWidth: (percent: number) => void;
  defaultWidth: number;
}) {
  /** Panel width (vw %) implied by a pointer at `clientX`. */
  const widthAt = (clientX: number) =>
    ((props.side === "left" ? clientX : window.innerWidth - clientX) / window.innerWidth) * 100;

  const onPointerDown = (event: PointerEvent & { currentTarget: HTMLDivElement }) => {
    if (event.button !== 0) return;
    event.preventDefault();
    const handle = event.currentTarget;
    handle.setPointerCapture(event.pointerId);
    // Keep the grab point under the cursor instead of snapping to the pointer.
    const offset = widthAt(event.clientX) - props.width();

    const onMove = (move: PointerEvent) => {
      // The release can happen outside the window and never reach us; end on the next move.
      if ((move.buttons & 1) === 0) return onEnd();
      props.setWidth(widthAt(move.clientX) - offset);
    };
    let ended = false;
    const onEnd = () => {
      if (ended) return;
      ended = true;
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onEnd);
      window.removeEventListener("pointercancel", onEnd);
      if (handle.hasPointerCapture(event.pointerId)) handle.releasePointerCapture(event.pointerId);
    };
    // Listen on window, not the handle: if capture is lost (e.g. the cursor leaves the app),
    // moves still arrive once it's back over any part of the window.
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onEnd);
    window.addEventListener("pointercancel", onEnd);
  };

  return (
    <div
      role="separator"
      aria-orientation="vertical"
      onPointerDown={onPointerDown}
      onDblClick={() => props.setWidth(props.defaultWidth)}
      class="relative z-20 -mx-1.5 w-3 shrink-0 cursor-col-resize touch-none"
    />
  );
}
