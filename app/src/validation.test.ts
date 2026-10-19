import { describe, expect, it } from "vitest";
import { canStart, checkPort } from "./validation";

describe("checkPort", () => {
  it("принимает порт 1–65535", () => {
    expect(checkPort("5000")).toEqual({ value: 5000, problem: null });
    expect(checkPort("1")).toEqual({ value: 1, problem: null });
    expect(checkPort("65535")).toEqual({ value: 65535, problem: null });
  });

  it("требует целое число", () => {
    expect(checkPort("").problem).toBe("port_format");
    expect(checkPort("http").problem).toBe("port_format");
    expect(checkPort("50.5").problem).toBe("port_format");
    expect(checkPort("-1").problem).toBe("port_format");
  });

  it("отвергает порт вне диапазона", () => {
    expect(checkPort("0").problem).toBe("port_range");
    expect(checkPort("65536").problem).toBe("port_range");
  });
});

describe("canStart", () => {
  it("запрещает старт без папки журналов", () => {
    expect(canStart("5000", "C:\\journals")).toBe(true);
    expect(canStart("5000", "   ")).toBe(false);
    expect(canStart("0", "C:\\journals")).toBe(false);
  });
});
