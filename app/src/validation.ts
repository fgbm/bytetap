/** Проверка порта: целое число 1–65535 (FR-25, AC3). */

export type PortProblem = "port_format" | "port_range";

/** Ключи локали для ошибки порта; проверяются тестом i18n. */
export const PORT_PROBLEM_KEYS: Record<PortProblem, string> = {
  port_format: "errors.port_format",
  port_range: "errors.port_range",
};

export interface PortCheck {
  value: number | null;
  problem: PortProblem | null;
}

/** `null` — поле пустое или не число; `port_range` — вне 1–65535. */
export function checkPort(raw: string): PortCheck {
  const text = raw.trim();
  if (text === "") return { value: null, problem: "port_format" };
  if (!/^\d+$/.test(text)) return { value: null, problem: "port_format" };
  const value = Number(text);
  if (value < 1 || value > 65535) return { value: null, problem: "port_range" };
  return { value, problem: null };
}

/** Конфигурация готова к старту, когда порт корректен и папка журналов указана. */
export function canStart(port: string, logDir: string): boolean {
  return checkPort(port).problem === null && logDir.trim() !== "";
}
