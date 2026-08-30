import { expect, test } from "@playwright/test";

// Phase 12 shipped a lot of behavior with no way to reach it. Shows is that front door, so a
// browser case asserts it is navigable rather than only that its component renders.
test("Shows is reachable and reports what an episode's release is waiting on", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: /^Shows$/ }).first().click();
  await expect(page.getByRole("heading", { name: "Shows" })).toBeVisible();

  await page.getByRole("button", { name: /^Inspect Creator update · Reel master$/ }).click();
  await expect(page.getByRole("heading", { name: "Release" })).toBeVisible();
  // A blocked member names its missing prerequisite rather than being quietly absent.
  await expect(page.getByText(/No line has been narrated yet, so there is no audio episode/i)).toBeVisible();
  await expect(page.getByRole("button", { name: "Open in Video Studio" })).toBeVisible();
});
