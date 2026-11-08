"""Tasks for the real-model eval.

Each graded run gets a fresh `World`, so tools can keep state (balances, a
write log) and fail on purpose. A run passes when the printed answer matches
and, for tasks with `expected_writes`, the world's write log matches exactly:
same write calls, same order, nothing extra.
"""

from __future__ import annotations

from dataclasses import dataclass, field


class ToolFailure(Exception):
    """Raised by a tool on purpose; Weave surfaces it as a ToolError."""


@dataclass(frozen=True)
class ToolDef:
    name: str
    params: tuple[str, ...]
    effect: str
    doc: str


@dataclass(frozen=True)
class EvalTask:
    id: str
    category: str
    instruction: str
    tools: tuple[ToolDef, ...]
    expected: str
    expected_writes: tuple[str, ...] | None = None


@dataclass
class World:
    balances: dict = field(default_factory=lambda: {"a1": 150, "a2": 20, "a3": 40})
    writes: list = field(default_factory=list)

    def _write(self, entry: str) -> None:
        self.writes.append(entry)

    # --- simple reads -------------------------------------------------
    def search(self, query):
        return f"hit:{query}"

    def summarize(self, a, b, c, d):
        return "|".join((a, b, c, d))

    def geo(self, city):
        return f"geo:{city}"

    def weather(self, place):
        return f"wx:{place}"

    def lookup_user(self, user_id):
        return f"user:{user_id}"

    def lookup_policy(self):
        return "policy:standard"

    def draft(self, user, policy):
        return f"{user}/{policy}"

    # --- chains --------------------------------------------------------
    def manager_of(self, employee_id):
        return {"e7": "e3", "e3": "e1", "e1": "e1"}.get(employee_id) or _fail(f"unknown employee {employee_id}")

    def office_of(self, employee_id):
        return {"e7": "o2", "e3": "o12", "e1": "o1"}.get(employee_id) or _fail(f"unknown employee {employee_id}")

    def city_of(self, office_id):
        return {"o1": "Austin", "o2": "Pune", "o12": "Lisbon"}.get(office_id) or _fail(f"unknown office {office_id}")

    # --- branching -----------------------------------------------------
    def stock(self, sku):
        return {"A1": 0, "B2": 7}.get(sku, 0)

    def restock_date(self, sku):
        return {"A1": "2026-12-01", "B2": "2026-11-20"}.get(sku, "unknown")

    def reserve(self, sku):
        self._write(f"reserve:{sku}")
        return f"res-{sku}"

    def forecast(self, city):
        return {"Oslo": "rain", "Cairo": "sun"}.get(city, "cloud")

    def gear_for(self, condition):
        return {"rain": "umbrella", "sun": "hat"}.get(condition, "jacket")

    # --- loops -----------------------------------------------------------
    def list_orders(self, customer_id):
        return {"c9": ["o1", "o2", "o3"]}.get(customer_id, [])

    def order_total(self, order_id):
        return {"o1": 12, "o2": 30, "o3": 7}[order_id]

    def list_tickets(self, queue):
        return {"q2": ["t4", "t8", "t15", "t16"]}.get(queue, [])

    def ticket_priority(self, ticket_id):
        return {"t4": "low", "t8": "high", "t15": "medium", "t16": "high"}[ticket_id]

    def find_docs(self, topic):
        return {"refund": ["d1", "d2", "d3"], "billing": ["d4"]}.get(topic, [])

    def doc_status(self, doc_id):
        return {"d1": "fresh", "d2": "stale", "d3": "stale", "d4": "fresh"}[doc_id]

    # --- tool errors -------------------------------------------------------
    def primary_price(self, sku):
        _fail("primary price service unavailable")

    def backup_price(self, sku):
        return {"Z9": "19.99"}.get(sku, "0.00")

    def get_email(self, user_id):
        emails = {"u1": "ana@ex.com", "u3": "raj@ex.com"}
        if user_id not in emails:
            _fail(f"no email on file for {user_id}")
        return emails[user_id]

    # --- writes ----------------------------------------------------------
    def charge(self, customer_id, amount):
        self._write(f"charge:{customer_id}:{amount}")
        return f"ch_{customer_id}_{amount}"

    def send_receipt(self, charge_id):
        self._write(f"receipt:{charge_id}")
        return f"rc_{charge_id}"

    def cancel_order(self, order_id):
        self._write(f"cancel:{order_id}")
        return "cancelled"

    def notify(self, customer_id, message):
        self._write(f"notify:{customer_id}:{message}")
        return "sent"

    def balance(self, account_id):
        return self.balances[account_id]

    def transfer(self, source, dest, amount):
        amount = int(amount)
        if self.balances[source] < amount:
            _fail(f"insufficient funds in {source}")
        self.balances[source] -= amount
        self.balances[dest] += amount
        self._write(f"transfer:{source}:{dest}:{amount}")
        return "ok"


def _fail(message: str):
    raise ToolFailure(message)


def _t(name, params, effect, doc):
    return ToolDef(name, tuple(params), effect, doc)


SEARCH = _t("search", ["query"], "read", "Search one query.")
TASKS = (
    EvalTask(
        "research_four", "parallel",
        "Search for q1, q2, q3, and q4 as four separate calls. Then summarize those four results in that order and print the summary.",
        (SEARCH, _t("summarize", ["a", "b", "c", "d"], "pure", "Join four search hits.")),
        "hit:q1|hit:q2|hit:q3|hit:q4",
    ),
    EvalTask(
        "weather_paris", "chain",
        "Geocode Paris, then fetch the weather for that place, and print the weather.",
        (_t("geo", ["city"], "read", "Turn a city name into a place id."),
         _t("weather", ["place"], "read", "Weather for a place id.")),
        "wx:geo:Paris",
    ),
    EvalTask(
        "support_draft", "parallel",
        "Look up user u1 and the policy at the same time. Draft a reply from those two results and print it.",
        (_t("lookup_user", ["user_id"], "read", "Load a user id."),
         _t("lookup_policy", [], "read", "Load the standard policy."),
         _t("draft", ["user", "policy"], "pure", "Draft a reply from a user and a policy.")),
        "user:u1/policy:standard",
    ),
    EvalTask(
        "skip_manager_city", "chain",
        "Find the manager of employee e7, then that manager's office, then the city of that office. Print only the city.",
        (_t("manager_of", ["employee_id"], "read", "Returns the employee id of someone's manager."),
         _t("office_of", ["employee_id"], "read", "Returns the office id where an employee sits."),
         _t("city_of", ["office_id"], "read", "Returns the city name of an office id.")),
        "Lisbon",
    ),
    EvalTask(
        "reserve_if_in_stock", "branch",
        "Check stock for sku A1. If stock is above 0, reserve it and print the reservation id. Otherwise do not reserve; print 'backorder ' followed by the restock date.",
        (_t("stock", ["sku"], "read", "Units in stock for a sku (integer)."),
         _t("restock_date", ["sku"], "read", "Next restock date for a sku."),
         _t("reserve", ["sku"], "reversible", "Reserve one unit; returns a reservation id.")),
        "backorder 2026-12-01", (),
    ),
    EvalTask(
        "reserve_in_stock", "branch",
        "Check stock for sku B2. If stock is above 0, reserve it and print the reservation id. Otherwise do not reserve; print 'backorder ' followed by the restock date.",
        (_t("stock", ["sku"], "read", "Units in stock for a sku (integer)."),
         _t("restock_date", ["sku"], "read", "Next restock date for a sku."),
         _t("reserve", ["sku"], "reversible", "Reserve one unit; returns a reservation id.")),
        "res-B2", ("reserve:B2",),
    ),
    EvalTask(
        "gear_for_forecast", "branch",
        "Get the forecast for Oslo and for Cairo. For each city, look up the gear for its forecast condition. Print 'Oslo:<gear> Cairo:<gear>'.",
        (_t("forecast", ["city"], "read", "Forecast condition word for a city."),
         _t("gear_for", ["condition"], "read", "Recommended gear for a condition word.")),
        "Oslo:umbrella Cairo:hat",
    ),
    EvalTask(
        "order_totals", "loop",
        "List the orders of customer c9, get the total of each order, and print the sum of the totals as a plain integer.",
        (_t("list_orders", ["customer_id"], "read", "List of order ids for a customer."),
         _t("order_total", ["order_id"], "read", "Integer total for one order.")),
        "49",
    ),
    EvalTask(
        "high_priority_tickets", "loop",
        "List the tickets in queue q2. Get the priority of each ticket. Print the ids of the tickets whose priority is 'high', in list order, joined by commas with no spaces.",
        (_t("list_tickets", ["queue"], "read", "List of ticket ids in a queue."),
         _t("ticket_priority", ["ticket_id"], "read", "Priority word of a ticket: low, medium, or high.")),
        "t8,t16",
    ),
    EvalTask(
        "stale_docs", "loop",
        "Find the docs about 'refund' and about 'billing'. Check the status of every doc found. Print the ids of the stale docs joined by commas with no spaces, or 'none' if there are none.",
        (_t("find_docs", ["topic"], "read", "List of doc ids about a topic."),
         _t("doc_status", ["doc_id"], "read", "Status of a doc: fresh or stale.")),
        "d2,d3",
    ),
    EvalTask(
        "price_fallback", "tool_error",
        "Get the price of sku Z9 from primary_price. If that call fails, get it from backup_price instead. Print only the price.",
        (_t("primary_price", ["sku"], "read", "Price from the primary service. May fail."),
         _t("backup_price", ["sku"], "read", "Price from the backup service.")),
        "19.99",
    ),
    EvalTask(
        "emails_with_failures", "tool_error",
        "Get the email of users u1, u2, and u3 with get_email. The tool fails for users with no email. Print the emails found, in user order, joined by ';', then a space, then how many lookups failed. Example format: 'a@x.com;b@x.com 1'.",
        (_t("get_email", ["user_id"], "read", "Email address of a user. Fails if none is on file."),),
        "ana@ex.com;raj@ex.com 1",
    ),
    EvalTask(
        "charge_then_receipt", "write_order",
        "Charge customer c1 the amount 25, then send a receipt for the charge id that charge returns. Print the receipt id.",
        (_t("charge", ["customer_id", "amount"], "irreversible", "Charge a customer; returns a charge id."),
         _t("send_receipt", ["charge_id"], "irreversible", "Email a receipt; returns a receipt id.")),
        "rc_ch_c1_25", ("charge:c1:25", "receipt:ch_c1_25"),
    ),
    EvalTask(
        "cancel_then_notify", "write_order",
        "Cancel order o5, and only after that notify customer c4 with the message 'order o5 cancelled'. Print 'done'.",
        (_t("cancel_order", ["order_id"], "irreversible", "Cancel an order."),
         _t("notify", ["customer_id", "message"], "irreversible", "Send a message to a customer.")),
        "done", ("cancel:o5", "notify:c4:order o5 cancelled"),
    ),
    EvalTask(
        "conditional_transfer", "write_order",
        "Read the balance of account a1. If it is at least 100, transfer 100 from a1 to a2. Then read the balance of a1 again and print it as a plain integer.",
        (_t("balance", ["account_id"], "read", "Current integer balance of an account."),
         _t("transfer", ["source", "dest", "amount"], "reversible", "Move an amount between accounts.")),
        "50", ("transfer:a1:a2:100",),
    ),
    EvalTask(
        "sweep_accounts", "write_order",
        "For each account in a2, a3: read its balance and, if it is above 30, transfer 30 from it to a1. Then print the balance of a1 as a plain integer.",
        (_t("balance", ["account_id"], "read", "Current integer balance of an account."),
         _t("transfer", ["source", "dest", "amount"], "reversible", "Move an amount between accounts.")),
        "180", ("transfer:a3:a1:30",),
    ),
)


def tool_lines(task: EvalTask) -> str:
    lines = []
    for tool in task.tools:
        params = ", ".join(tool.params)
        lines.append(f"{tool.name}({params}), effect {tool.effect}. {tool.doc}")
    return "\n".join(lines)


def tool_fn(world: World, name: str):
    return getattr(world, name)


def call_tool(world: World, name: str, args: list):
    return tool_fn(world, name)(*args)


def render(value) -> str:
    """How a tool result is shown to a model outside Weave."""
    import json

    return value if isinstance(value, str) else json.dumps(value)
