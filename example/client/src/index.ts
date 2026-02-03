const SERVER_URL = process.env.SERVER_URL || "http://localhost:3000";
const COOLDOWN_MS = parseInt(process.env.COOLDOWN_MS || "2000", 10);
const MAX_ITERATIONS = parseInt(process.env.MAX_ITERATIONS || "5", 10);

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function waitForServer(): Promise<void> {
  console.log(`Waiting for server at ${SERVER_URL}...`);
  const maxAttempts = 30;
  for (let i = 0; i < maxAttempts; i++) {
    try {
      const res = await fetch(`${SERVER_URL}/health`);
      if (res.ok) {
        console.log("Server is ready");
        return;
      }
    } catch {
      // Server not ready yet
    }
    await sleep(1000);
  }
  throw new Error("Server did not become ready in time");
}

async function createItem(name: string): Promise<void> {
  console.log(`POST /items - Creating item: ${name}`);
  const res = await fetch(`${SERVER_URL}/items`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name }),
  });

  if (!res.ok) {
    const text = await res.text();
    console.error(`Failed to create item: ${res.status} ${text}`);
    return;
  }

  const item = await res.json();
  console.log(`Created: ${JSON.stringify(item)}`);
}

async function listItems(): Promise<void> {
  console.log("GET /items - Listing items");
  const res = await fetch(`${SERVER_URL}/items`);

  if (!res.ok) {
    const text = await res.text();
    console.error(`Failed to list items: ${res.status} ${text}`);
    return;
  }

  const items = await res.json();
  console.log(`Items (${items.length}): ${JSON.stringify(items)}`);
}

async function main() {
  await waitForServer();

  for (let i = 1; i <= MAX_ITERATIONS; i++) {
    console.log(`\n--- Iteration ${i}/${MAX_ITERATIONS} ---`);

    await createItem(`item-${i}`);
    await sleep(COOLDOWN_MS);

    await listItems();
    await sleep(COOLDOWN_MS);
  }

  console.log("\nClient finished all iterations");
}

main().catch((err) => {
  console.error("Client error:", err);
  process.exit(1);
});
