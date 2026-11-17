import { ChatHeader } from "../components/chat-header";
import { ChatView } from "../components/chat-view";
import { PromptBox } from "../components/prompt-box";

/** New chat / active session view. */
export function HomePage() {
  return (
    <div class="flex h-full flex-col">
      <ChatHeader />

      <ChatView />

      {/* Pinned to the bottom, full width of the panel. */}
      <div class="shrink-0 p-4">
        <PromptBox />
      </div>
    </div>
  );
}
