/**
 * Разбор текста обмена на плашки управляющих символов (FR-22).
 *
 * Общий модуль приложения и сайта: пример обмена на главной странице документации
 * и тест в `app/` пользуются одной функцией.
 */

/** Управляющие символы, записанные как `<STX>`, `<CR>`, `<x0B>` и т.п. */
const CONTROL = /(<(?:STX|ETX|ETB|EOT|ENQ|ACK|NAK|VT|FS|CR|LF|x[0-9A-Fa-f]{2})>)/g;

export interface Segment {
  chip: boolean;
  value: string;
}

/**
 * Режет текст на обычные куски и плашки управляющих символов.
 *
 * `String.split` с захватывающей группой кладёт совпадения на нечётные индексы,
 * а текст между ними — на чётные; поэтому плашкой считается только совпадение,
 * а фрагменты вида `<foo>` остаются текстом.
 */
export function segments(text: string): Segment[] {
  return text
    .split(CONTROL)
    .map((value, index) => ({ chip: index % 2 === 1, value }))
    .filter((segment) => segment.value !== "");
}
