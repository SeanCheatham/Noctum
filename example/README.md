# Noctum Example: Client-Server with Database

This example demonstrates Noctum's ability to analyze a multi-service application and generate the appropriate Docker Compose configuration for sandboxed simulation testing.

## Services

### Server

A simple Express API with two endpoints:

- `POST /items` - Create an item with a `name` field
- `GET /items` - List all items

The server connects to a PostgreSQL database via the `DATABASE_URL` environment variable. Note that **no database service is defined** in this directory—Noctum will detect the dependency and generate one.

### Client

A simple client that runs in a loop:

1. Creates an item via POST
2. Waits for a cooldown period
3. Lists all items via GET
4. Waits for a cooldown period
5. Repeats for a configurable number of iterations

## What Noctum Should Generate

When you run Noctum against this example, it should:

1. Analyze both services and detect their dependencies
2. Recognize that the server requires a PostgreSQL database
3. Generate a `docker-compose.yml` with:
   - A `db` service (PostgreSQL)
   - The `server` service, configured with `DATABASE_URL` pointing to the db
   - The `client` service, configured with `SERVER_URL` pointing to the server
   - Proper health checks and dependency ordering
   - An internal sandbox network with no external access

## Running the Example

```bash
# From the Noctum root directory
npx noctum serve --dir ./example
```

Then open the web UI and follow the onboarding flow. Noctum will generate the necessary Docker configuration and run the simulation.
