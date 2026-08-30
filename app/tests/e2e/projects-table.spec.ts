import { expect, test } from "@playwright/test";

// A video production and an audio production are different experiences to open but the same thing
// to look for, so both belong in one table rather than in separate lists on the same page.
test("every production is in one table that opens its own workspace", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: /^Projects$/ }).first().click();

  const table = page.getByRole("table");
  await expect(table).toBeVisible();
  await expect(page.getByRole("button", { name: /^Open Creator update · Reel master$/ })).toBeVisible();
  await expect(page.getByRole("button", { name: /^Open Local AI, Close to Home$/ })).toBeVisible();

  // Opening an audio production replaces the library with that project's own screen.
  await page.getByRole("button", { name: /^Open Local AI, Close to Home$/ }).click();
  await expect(page.getByRole("region", { name: "Project workspace" })).toBeVisible();
  await expect(page.getByRole("table")).toHaveCount(0);
  await page.getByRole("button", { name: /All projects/ }).click();
  await expect(page.getByRole("table")).toBeVisible();

  // Filtering narrows the same table rather than switching to a different surface.
  await page.getByRole("radio", { name: "Video" }).click();
  await expect(page.getByRole("button", { name: /^Open Local AI, Close to Home$/ })).toHaveCount(0);
  await expect(page.getByRole("button", { name: /^Open Creator update · Reel master$/ })).toBeVisible();
});
