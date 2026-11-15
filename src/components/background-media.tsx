import { type JSX, Show } from "solid-js";
import { background, markBackgroundReady } from "../lib/background";

/** Renders the current background (image or looping muted video) covering its box. */
export function BackgroundMedia(props: { class?: string; style?: JSX.CSSProperties }) {
  return (
    <Show when={background()}>
      {(current) => (
        <Show
          when={current().kind === "video"}
          fallback={
            <img
              src={current().src}
              alt=""
              draggable={false}
              onLoad={() => markBackgroundReady(current().src)}
              onError={() => markBackgroundReady(current().src)}
              class={`object-cover ${props.class ?? ""}`}
              style={props.style}
            />
          }
        >
          <video
            src={current().src}
            autoplay
            loop
            muted
            playsinline
            disablepictureinpicture
            onCanPlay={() => markBackgroundReady(current().src)}
            onError={() => markBackgroundReady(current().src)}
            class={`pointer-events-none object-cover ${props.class ?? ""}`}
            style={props.style}
          />
        </Show>
      )}
    </Show>
  );
}
