import express from "express";
import pg from "pg";

const app = express();
app.use(express.json());

const pool = new pg.Pool({
  connectionString: process.env.DATABASE_URL,
});

async function initDb() {
  await pool.query(`
    CREATE TABLE IF NOT EXISTS items (
      id SERIAL PRIMARY KEY,
      name TEXT NOT NULL,
      created_at TIMESTAMP DEFAULT NOW()
    )
  `);
}

app.get("/health", (_req, res) => {
  res.json({ status: "ok" });
});

app.get("/items", async (_req, res) => {
  try {
    const result = await pool.query("SELECT * FROM items ORDER BY id DESC");
    res.json(result.rows);
  } catch (err) {
    console.error("Failed to fetch items:", err);
    res.status(500).json({ error: "Database error" });
  }
});

app.post("/items", async (req, res) => {
  const { name } = req.body;
  if (!name || typeof name !== "string") {
    res.status(400).json({ error: "Name is required" });
    return;
  }

  try {
    const result = await pool.query(
      "INSERT INTO items (name) VALUES ($1) RETURNING *",
      [name]
    );
    console.log(`Created item: ${JSON.stringify(result.rows[0])}`);
    res.status(201).json(result.rows[0]);
  } catch (err) {
    console.error("Failed to create item:", err);
    res.status(500).json({ error: "Database error" });
  }
});

const port = parseInt(process.env.PORT || "3000", 10);

initDb()
  .then(() => {
    app.listen(port, () => {
      console.log(`Server listening on port ${port}`);
    });
  })
  .catch((err) => {
    console.error("Failed to initialize database:", err);
    process.exit(1);
  });
