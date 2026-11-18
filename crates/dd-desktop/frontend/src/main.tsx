import { createSignal } from "solid-js";
import { render } from "@solidjs/web";
import { invoke } from "@tauri-apps/api/core";
import "./style.css";

type Command = "providers_hello" | "tools_hello";

function App() {
  const [result, setResult] = createSignal("Нажмите кнопку");

  const request = async (command: Command) => {
    try {
      setResult(await invoke<string>(command));
    } catch (e) {
      setResult(`Ошибка: ${e}`);
    }
  };

  return (
    <main>
      <h1>dd</h1>
      <div class="buttons">
        <button onClick={() => request("providers_hello")}>Запрос в dd-providers</button>
        <button onClick={() => request("tools_hello")}>Запрос в dd-tools</button>
      </div>
      <pre class="result">{result()}</pre>
    </main>
  );
}

render(() => <App />, document.getElementById("root")!);
