import { locale } from "./i18n";

const UNITS = ["byte", "kilobyte", "megabyte", "gigabyte", "terabyte"] as const;

/** Human-readable size in the current locale, e.g. "1.2 GB" / "1,2 ГБ". */
export function formatBytes(bytes: number): string {
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024;
    unit++;
  }
  return new Intl.NumberFormat(locale(), {
    style: "unit",
    unit: UNITS[unit],
    unitDisplay: "short",
    maximumFractionDigits: unit >= 3 ? 1 : 0,
  }).format(value);
}

/** Transfer rate in the current locale, e.g. "12.3 MB/s" / "12,3 МБ/с". */
export function formatSpeed(bytesPerSecond: number): string {
  let value = bytesPerSecond;
  let unit = 0;
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024;
    unit++;
  }
  return new Intl.NumberFormat(locale(), {
    style: "unit",
    unit: `${UNITS[unit]}-per-second`,
    unitDisplay: "short",
    maximumFractionDigits: unit >= 2 ? 1 : 0,
  }).format(value);
}

/** Compact count in the current locale, e.g. "17M" / "17 млн". */
export const formatCount = (count: number) =>
  new Intl.NumberFormat(locale(), { notation: "compact", maximumFractionDigits: 1 }).format(count);
