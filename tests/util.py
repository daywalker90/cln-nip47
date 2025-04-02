import logging
import os
import random
import string
from pathlib import Path

import pytest

RUST_PROFILE = os.environ.get("RUST_PROFILE", "debug")
COMPILED_PATH = Path.cwd() / "target" / RUST_PROFILE / "cln-nip47"
DOWNLOAD_PATH = Path.cwd() / "tests" / "cln-nip47"


@pytest.fixture
def get_plugin(directory):
    if COMPILED_PATH.is_file():
        return COMPILED_PATH
    elif DOWNLOAD_PATH.is_file():
        return DOWNLOAD_PATH
    else:
        raise ValueError("No files were found.")


def generate_random_label():
    label_length = 8
    random_label = "".join(
        random.choice(string.ascii_letters) for _ in range(label_length)
    )
    return random_label


def generate_random_number():
    return random.randint(1, 20_000_000_000_000_00_000)


def pay_with_thread(rpc, bolt11):
    LOGGER = logging.getLogger(__name__)
    try:
        rpc.dev_pay(bolt11, dev_use_shadow=False)
    except Exception as e:
        LOGGER.info(f"holdinvoice: Error paying payment hash:{e}")
        pass


def update_config_file_option(lightning_dir, option_name, option_value):
    with open(lightning_dir + "/config", "r") as file:
        lines = file.readlines()

    for i, line in enumerate(lines):
        if line.startswith(option_name):
            lines[i] = option_name + "=" + option_value + "\n"

    with open(lightning_dir + "/config", "w") as file:
        file.writelines(lines)


def experimental_anchors_check(node_factory):
    l1 = node_factory.get_node()
    version = l1.rpc.getinfo()["version"]
    if version.startswith("v23"):
        return True
    else:
        return False
