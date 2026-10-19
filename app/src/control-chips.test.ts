import { describe, expect, it } from "vitest";
import { segments } from "../../shared/control-chips";

describe("segments", () => {
  it("делает плашками управляющие символы, а не любой текст в угловых скобках", () => {
    expect(segments("<STX><foo><ETX>")).toEqual([
      { chip: true, value: "<STX>" },
      { chip: false, value: "<foo>" },
      { chip: true, value: "<ETX>" },
    ]);
  });

  it("возвращает текст без управляющих символов одним куском", () => {
    expect(segments("MSH|^~\\&")).toEqual([{ chip: false, value: "MSH|^~\\&" }]);
  });
});
