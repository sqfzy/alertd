#!/usr/bin/python3
import time


def main() -> None:
    counter = 0
    with open("/dev/shm/alertd_test_counter", "r+b", buffering=0) as shared_memory:
        while True:
            counter += 1
            shared_memory.seek(0)
            shared_memory.write(counter.to_bytes(8, "little"))
            time.sleep(5)


if __name__ == "__main__":
    main()

