# License of the RFAL sources in this directory

The files in `rfal/` are STMicroelectronics' **RFAL (RF Abstraction Layer)
v4.0.2** (STSW-ST25RFAL004).

    COPYRIGHT 2016-2020 STMicroelectronics, all rights reserved

They are **not** covered by the MIT / Apache-2.0 license of the
`st25r3916-rfal` Rust crate. They are redistributed unmodified, except for
the changes listed in [`MODIFICATIONS.md`](MODIFICATIONS.md), solely for use
with STMicroelectronics ST25R NFC devices, under STMicroelectronics' license
terms, which are available from
<https://www.st.com/en/embedded-software/stsw-st25rfal004.html>.

As stated in the source file headers: unless required by applicable law or
agreed to in writing, the software is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied, AND
SPECIFICALLY DISCLAIMING THE IMPLIED WARRANTIES OF MERCHANTABILITY, FITNESS
FOR A PARTICULAR PURPOSE, AND NON-INFRINGEMENT.

The platform configuration header used to build these sources
(`src/c/rfal_platform.h`) and the freestanding `src/c/freestanding/string.h`
shim are part of the Rust crate and licensed under MIT OR Apache-2.0.
