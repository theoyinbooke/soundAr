import { describe, expect, it } from "vitest";
import { videoSourceWithFirstFrame } from "./videoPlayback";

describe("videoSourceWithFirstFrame", () => {
  it("requests the real opening frame once", () => {
    expect(videoSourceWithFirstFrame("asset://localhost/master.mp4")).toBe("asset://localhost/master.mp4#t=0.001");
    expect(videoSourceWithFirstFrame("asset://localhost/master.mp4#t=0.001")).toBe("asset://localhost/master.mp4#t=0.001");
    expect(videoSourceWithFirstFrame()).toBeUndefined();
  });
});
