import { ChatHeader } from "../components/chat-header";
import { PromptBox } from "../components/prompt-box";

/** New chat / active session view. */
export function HomePage() {
  return (
    <div class="flex h-full flex-col">
      <ChatHeader />

      {/* Message area; empty until the chat view exists. */}
      <div class="thin-scroll min-h-0 flex-1 overflow-y-auto" />

      {/* Pinned to the bottom, full width of the panel. */}
      <div class="shrink-0 p-4">
        <PromptBox />
      </div>
    </div>
  );
}
