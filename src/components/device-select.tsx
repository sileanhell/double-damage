import { t } from "../lib/i18n";
import { CPU, modelDevice, setModelDevice } from "../lib/model-device";
import { llamaDevices } from "../lib/providers";
import { Dropdown } from "./dropdown";

/** "NVIDIA GeForce RTX 4060" → "RTX 4060": the vendor is implied by the backend and the list. */
const shortName = (name: string) =>
  name
    .replace(/^NVIDIA\s+/i, "")
    .replace(/^GeForce\s+/i, "")
    .replace(/^AMD\s+/i, "")
    .replace(/^Intel\(R\)\s+/i, "Intel ");

/** Picks the GPU + backend (or CPU) a local model runs on. */
export function DeviceSelect(props: { model: string }) {
  const options = () => {
    const devices = llamaDevices();
    return [
      ...devices.map((device) => {
        // Two identical cards: tell them apart by their index.
        const twin = devices.filter((other) => other.backend === device.backend && other.name === device.name).length > 1;
        const index = twin ? ` #${device.id.match(/\d+$/)?.[0] ?? ""}` : "";
        return { value: device.id, label: `${shortName(device.name)}${index} · ${device.backend}` };
      }),
      { value: CPU, label: t("models.cpu") },
    ];
  };

  return (
    <Dropdown
      value={modelDevice(props.model)}
      options={options()}
      onChange={(device) => setModelDevice(props.model, device)}
      title={t("models.runOn")}
      variant="outline"
      placement="bottom-start"
    />
  );
}
