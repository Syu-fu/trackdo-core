"""Writes rrule_reference.txt: random day-level recurrence rules and the dates
python-dateutil 2.9.0.post0 computes for them.

Each line holds a start date, a rule and its first occurrences up to
HORIZON, at most LIMIT of them. RFC 5545 counts the start as the first
occurrence even when the rule does not name it, while dateutil leaves it out,
so the start is added here and COUNT is applied afterwards. dateutil also
admits only days matching both plain and numbered weekdays of BYDAY, so the
rules here never mix the two.

    python3 -m venv venv && venv/bin/pip install python-dateutil==2.9.0.post0
    venv/bin/python generate_rrule_reference.py > rrule_reference.txt
"""

import random
import signal
from datetime import date, datetime, timedelta
from itertools import islice

from dateutil.rrule import rrulestr

CASES = 400
LIMIT = 12
HORIZON = datetime(2080, 12, 31)
WEEKDAYS = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"]


def signed(rng, top, size):
    values = {rng.choice([-1, 1]) * rng.randint(1, top) for _ in range(size)}
    return ",".join(str(v) for v in sorted(values))


def random_rule(rng, start):
    freq = rng.choice(["DAILY", "WEEKLY", "MONTHLY", "YEARLY"])
    parts = [f"FREQ={freq}"]
    if rng.random() < 0.4:
        parts.append(f"INTERVAL={rng.randint(2, 4)}")
    end = rng.random()
    if end < 0.3:
        parts.append(f"COUNT={rng.randint(1, 15)}")
    elif end < 0.5:
        until = start + timedelta(days=rng.randint(0, 3 * 365))
        parts.append(f"UNTIL={until:%Y%m%d}")
    by_parts = []
    if rng.random() < 0.3:
        months = sorted(rng.sample(range(1, 13), rng.randint(1, 3)))
        by_parts.append("BYMONTH=" + ",".join(map(str, months)))
    week_no = freq == "YEARLY" and rng.random() < 0.2
    if week_no:
        by_parts.append("BYWEEKNO=" + signed(rng, 53, rng.randint(1, 2)))
    if freq == "YEARLY" and rng.random() < 0.2:
        by_parts.append("BYYEARDAY=" + signed(rng, 366, rng.randint(1, 3)))
    if freq != "WEEKLY" and rng.random() < 0.35:
        by_parts.append("BYMONTHDAY=" + signed(rng, 31, rng.randint(1, 3)))
    if rng.random() < 0.5:
        numbered = freq == "MONTHLY" or (freq == "YEARLY" and not week_no)
        within_month = freq == "MONTHLY" or any(p.startswith("BYMONTH=") for p in by_parts)
        # dateutil requires plain and numbered weekdays to match together,
        # where RFC 5545 takes either, so a rule uses only one kind.
        numbered = numbered and rng.random() < 0.5
        days = set()
        for _ in range(rng.randint(1, 3)):
            code = rng.choice(WEEKDAYS)
            if numbered:
                top = 5 if within_month else 53
                code = f"{rng.choice([-1, 1]) * rng.randint(1, top)}{code}"
            days.add(code)
        by_parts.append("BYDAY=" + ",".join(sorted(days)))
    if by_parts and rng.random() < 0.25:
        by_parts.append("BYSETPOS=" + signed(rng, 3, rng.randint(1, 2)))
    if rng.random() < 0.3:
        by_parts.append(f"WKST={rng.choice(WEEKDAYS)}")
    return ";".join(parts + by_parts)


def occurrences(rule, start):
    count = None
    parts = []
    for part in rule.split(";"):
        if part.startswith("COUNT="):
            count = int(part[len("COUNT="):])
        else:
            parts.append(part)
    begin = datetime.combine(start, datetime.min.time())
    later = (
        d.date()
        for d in rrulestr(";".join(parts), dtstart=begin)
        if d > begin
    )
    until = next((p[len("UNTIL="):] for p in parts if p.startswith("UNTIL=")), None)
    first = [start] if until is None or f"{start:%Y%m%d}" <= until else []
    dates = first + list(islice((d for d in later if d <= HORIZON.date()), LIMIT))
    if count is not None:
        dates = dates[:count]
    return [d for d in dates if d <= HORIZON.date()][:LIMIT]


class Slow(Exception):
    pass


def on_alarm(signum, frame):
    raise Slow()


def main():
    rng = random.Random(5545)
    signal.signal(signal.SIGALRM, on_alarm)
    written = 0
    while written < CASES:
        start = date(1990, 1, 1) + timedelta(days=rng.randint(0, 40 * 365))
        rule = random_rule(rng, start)
        signal.alarm(2)
        try:
            dates = occurrences(rule, start)
        except Slow:
            # Rules that never match scan up to year 9999; skip them.
            continue
        finally:
            signal.alarm(0)
        print(f"{start:%Y%m%d} {rule} {','.join(f'{d:%Y%m%d}' for d in dates)}")
        written += 1


if __name__ == "__main__":
    main()
