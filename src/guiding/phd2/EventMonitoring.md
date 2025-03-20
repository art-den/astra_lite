# Introduction #

PHD2 allows programs to actively monitor guiding status and control PHD2. This allows clients to do things like:
  * graph PHD2 guide data in real time
  * monitor PHD2 for exceptional events like guiding lost, and send an alert when something goes wrong
  * control PHD2 from an imaging application

This document describes the communication protocol for interacting programmatically with PHD2.

If you are looking for ready-to-use code that implements this communication protocol, please see this repo [PHD2 API Client Code](https://github.com/agalasso/phd2client).

# Connection #

Clients connect to PHD2 on TCP port 4400. When multiple PHD instances are running, each instance listens on successive port numbers (4401, 4402, ...).

PHD allows multiple clients to establish connections simultaneously.

When a client establishes a connection, PHD sends a series of event notification messages to the client (see [#Initial\_Messages](#initial-event-messages)). Then, as guiding events take place in PHD, notification messages are sent to all connected clients.

# Event Notification Messages #

## Event Notification Message Format ##

Event Notification messages are formatted as [JSON](http://www.json.org/) objects. Each message is a single line of text terminated by CR LF.

## Common attributes ##

All messages contain the following attributes in common:

| Attribute | Type | Description |
|:----------|:-----|:------------|
|`Event`    | string | the name of the event |
| `Timestamp` | number | the timesamp of the event in seconds from the epoch, including fractional seconds |
| `Host`    | string | the hostname of the machine running PHD |
| `Inst`    | number | the PHD instance number (1-based) |

## Event Message Descriptions ##

Here are the available messages and the attributes of each message.

### `Version` ###

Describes the PHD and message protocol versions.

| Attribute | Type | Description |
|:----------|:-----|:------------|
| `PHDVersion` | string | the PHD version number |
| `PHDSubver` | string | the PHD sub-version number |
| `MsgVersion` | number | the version number of the event message protocol. The current version is 1. We will bump this number if the message protocol changes. |
| `OverlapSupport` | boolean | true if PHD support receiving RPC order while previous order has not been completed (default for latest version) |

Example
```
{"Event":"Version","Timestamp":1372082668.897,"Host":"AGALASSO","Inst":1,"PHDVersion":"2.0.4","PHDSubver":"a","MsgVersion":1}
```

### `LockPositionSet` ###

The lock position has been established.

| Attribute | Type | Description |
|:----------|:-----|:------------|
| X         | number | lock position X-coordinate |
| Y         | number | lock position Y-coordinate |

### `Calibrating` ###

Calibration step

| Attribute | Type | Description |
|:----------|:-----|:------------|
| Mount     | string | name of the mount that was calibrated |
| dir       | string | calibration direction (phase) |
| dist      | number | distance from starting location |
| dx        | number | x offset from starting position |
| dy        | number | y offset from starting position |
| pos       | [number,number] | star coordinates |
| step      | number | step number |
| State     | string | calibration status message |

### `CalibrationComplete` ###

Calibration completed successfully.

| Attribute | Type | Description |
|:----------|:-----|:------------|
| Mount     | string | name of the mount that was calibrated |

### `StarSelected` ###

A star has been selected.

| Attribute | Type | Description |
|:----------|:-----|:------------|
| X         | number | lock position X-coordinate |
| Y         | number | lock position Y-coordinate |

### `StartGuiding` ###

Guiding begins.

(no message attributes)

### `Paused` ###

Guiding has been paused.

| Attribute | Type | Description |
|:----------|:-----|:------------|

### `StartCalibration` ###

Calibration begins.

| Attribute | Type | Description |
|:----------|:-----|:------------|
| Mount     | string | the name of the mount being calibrated |

### `AppState` ###

| Attribute | Type | Description |
|:----------|:-----|:------------|
| State     | string | the current state of PHD |

The state attribute can be one of the following:

| State | Description |
|:------|:------------|
| Stopped | PHD is idle |
| Selected | A star is selected but PHD is neither looping exposures, calibrating, or guiding |
| Calibrating | PHD is calibrating |
| Guiding | PHD is guiding |
| LostLock  | PHD is guiding, but the frame was dropped |
| Paused | PHD is paused |
| Looping | PHD is looping exposures |

The `AppState` notification is only sent when the client first connects to PHD2 (see [#Initial\_Messages](#initial-event-messages)). If an application would like to maintain an up-to-date `AppState` status, it will need to update its notion of `AppState` by handling individual notification events as follows:

| Event | New AppState |
|:------|:------------|
|AppState|\<State\>|
|GuideStep|Guiding|
|Paused|Paused|
|StartCalibration|Calibrating|
|LoopingExposures|Looping|
|LoopingExposuresStopped|Stopped|
|StarLost|LostLock|

### `CalibrationFailed` ###

Calibration failed.

| Attribute | Type | Description |
|:----------|:-----|:------------|
| Reason    | string | an error message string |

### `CalibrationDataFlipped` ###

Calibration data has been flipped.

| Attribute | Type | Description |
|:----------|:-----|:------------|
| Mount     | string | the name of the mount |

### `LockPositionShiftLimitReached` ###

The lock position shift is active and the lock position has shifted to the edge of the field of view.

(no event attributes)

### `LoopingExposures` ###

Sent for each exposure frame while looping exposures.

| Attribute | Type | Description |
|:----------|:-----|:------------|
| Frame     | number | the exposure frame number; starts at 1 each time looping starts |

### `LoopingExposuresStopped` ###

Looping exposures has stopped.

(no event attributes)

### `SettleBegin` ###

Sent when settling begins after a `dither` or `guide` method invocation.

(no event attributes)

### `Settling` ###

Sent for each exposure frame after a `dither` or `guide` method invocation until guiding has settled.

| Attribute | Type | Description |
|:----------|:-----|:------------|
| Distance  | number | the current distance between the guide star and lock position |
| Time      | number | the elapsed time that the distance has been below the settling tolerance distance (the `pixels` attribute of the `SETTLE` parameter) |
| SettleTime | number | the requested settle time (the `time` attribute of the `SETTLE` parameter) |
| StarLocked | boolean | true if the guide star was found in the current camera frame, false if the guide star was lost |

### `SettleDone` ###

Sent after a `dither` or `guide` method invocation indicating whether settling was achieved, or if the guider failed to settle before the time limit was reached, or if some other error occurred preventing `guide` or `dither` to complete and settle.

| Attribute | Type | Description |
|:----------|:-----|:------------|
| Status    | number | 0 if settling succeeded, non-zero if it failed |
| Error     | string | a description of the reason why the `guide` or `dither` command failed to complete and settle |
| TotalFrames | number | the number of camera frames while settling |
| DroppedFrames | number | the number of dropped camera frames (guide star not found) while settling |

### `StarLost` ###

A frame has been dropped due to the star being lost.

| Attribute | Type | Description |
|:----------|:-----|:------------|
| Frame     | number | frame number |
| Time      | number | time since guiding started, seconds |
| StarMass  | number | star mass value |
| SNR       | number | star SNR value |
| AvgDist   | number |a smoothed average of the guide distance in pixels (equivalent to value returned by socket server MSG\_REQDIST)|
| ErrorCode | number | error code  |
| Status    | string | error message |

### `GuidingStopped` ###

Guiding has stopped.

(no event attributes)

### `Resumed` ###

PHD has been resumed after having been paused.

(no attributes)

### `GuideStep` ###

This event corresponds to a line in the PHD Guide Log. The event is sent for each frame while guiding.

| Attribute | Type | Description |
|:----------|:-----|:------------|
|Frame      |number|The frame number; starts at 1 each time guiding starts|
|Time       |number| the time in seconds, including fractional seconds, since guiding started|
|Mount      |string|the name of the mount|
|dx         |number|the X-offset in pixels|
|dy         |number|the Y-offset in pixels|
|RADistanceRaw|number|the RA distance in pixels of the guide offset vector|
|DECDistanceRaw|number|the Dec distance in pixels of the guide offset vector|
|RADistanceGuide|number|the guide algorithm-modified RA distance in pixels of the guide offset vector|
|DECDistanceGuide|number|the guide algorithm-modified Dec distance in pixels of the guide offset vector|
|RADuration |number|the RA guide pulse duration in milliseconds|
|RADirection|string|"East" or "West"   |
|DECDuration|number|the Dec guide pulse duration in milliseconds|
|DECDirection|string|"South" or "North"   |
|StarMass   |number|the Star Mass value of the guide star|
|SNR        |number|the computed Signal-to-noise ratio of the guide star|
|HFD        |number|the guide star half-flux diameter (HFD) in pixels|
|AvgDist    |number|a smoothed average of the guide distance in pixels (equivalent to value returned by socket server MSG\_REQDIST)|
|RALimited  |boolean|true if step was limited by the Max RA setting (attribute omitted if step was not limited)|
|DecLimited |boolean|true if step was limited by the Max Dec setting (attribute omitted if step was not limited)|
|ErrorCode  |number|the star finder error code, 1=saturated, 2=low SNR, 3=low mass, 4=low HFD, 5=High HFD, 6=edge of frame, 7=mass change, 8=unexpected|

### `GuidingDithered` ###

The lock position has been dithered.

| Attribute | Type | Description |
|:----------|:-----|:------------|
|dx         |number|the dither X-offset in pixels|
|dy         |number|the dither Y-offset in pixels|

### `LockPositionLost` ###

The lock position has been lost.

(no attributes)

### `Alert` ###

An alert message was displayed in PHD2.

| Attribute | Type | Description |
|:----------|:-----|:------------|
|Msg        |string|the text of the alert message|
|Type       |string|The type of alert: "info", "question", "warning", or "error"|

### `GuideParamChange` ###

A guiding parameter has been changed.

| Attribute | Type | Description |
|:----------|:-----|:------------|
|Name       |string|the name of the parameter that changed|
|Value      |any|the new value of the parameter|

### `ConfigurationChange` ###

Notification sent when any settings are changed -- allows a client to keep in sync with PHD2 configuration settings by exporting settings only when required.

(no event attributes)

## Initial Event Messages ##

When a client first connects, PHD sends a series of event messages to the client. The first event is
  * `Version`
Then, one or more of
  * `LockPositionSet`
  * `StarSelected`
  * `CalibrationComplete`
  * `StartGuiding`, `StartCalibration`, or `Paused`
depending on the state of PHD.

Finally, PHD will send
  * `AppState`

# PHD Server method invocation #

PHD2 provides an RPC (remote procedure call) interface for event server clients. The message protocol is [JSON RPC 2.0](http://www.jsonrpc.org/specification).

Requests are sent as a single line of text, terminated by `CR LF`. Responses from the server are also a single line of text terminated by `CR LF`.

Here is an example exchange between client (`-->`) and server (`<--`):

Get current camera exposure time:
```
--> {"method": "get_exposure", "id": 1}
<-- {"jsonrpc": "2.0", "result": 1000, "id": 1}
```

Set camera exposure:
```
--> {"method": "set_exposure", "params": [1500], "id": 2}
<-- {"jsonrpc": "2.0", "result": 0, "id": 2}
```

Set camera exposure (error):
```
--> {"method": "set_exposure", "params": [1502], "id": 3}
<-- {"jsonrpc": "2.0", "error": {"code": 1, "message": "could not set exposure duration"}, "id": 3}
```

## Available Methods ##

|**Method**|**params**|**result**|**Description**|
|:---------|:---------|:---------|:--------------|
|`capture_single_frame`|exposure: exposure duration milliseconds (optional), subframe: array [x,y,width,height] (optional)| integer(0) | captures a singe frame; guiding and looping must be stopped first |
|`clear_calibration`|string: "mount" or "ao" or "both"| integer (0) | if parameter is omitted, will clear both mount and AO. Clearing calibration causes PHD2 to recalibrate next time guiding starts.|
|`dither`  | `amount`:float, amount in pixels<br>`raOnly`: boolean, default=false;<br>`settle`: object | integer (0) | See below     |
|`find_star`|`roi`: [x,y,width,height], optional, default = use full frame      | on success: returns the lock position of the selected star, otherwise returns an error object | Auto-select a star |
|`flip_calibration`|none      | integer (0) |               |
|`get_algo_param_names`|string: axis ("ra","x","dec", or "y") | array of guide algorithm param names (strings) |  |
|`get_algo_param`|string: axis, string: name    | float: the value of the named parameter  | get the value of a guide algorithm parameter on an axis |
|`get_app_state`|none      |string: current app state| same value that came in the last [AppState](EventMonitoring#AppState.md) notification |
|`get_camera_frame_size`|none      |array: [width,height]| camera frame size in pixels. Note: frame size is only guaranteed to be available after at least one frame has been received from the camera |
|`get_calibrated`|none      |boolean: true if calibrated|               |
|`get_calibration_data`|string: which ("AO" or "Mount") optional, default = "Mount"| example output: `  {"calibrated":true,"xAngle":-167.1,"xRate":39.124,"xParity":"-","yAngle":106.1,"yRate":39.330,"yParity":"+"} ` |
|`get_connected`|none      |boolean: true if all equipment is connected |               |
|`get_cooler_status`|none      |"temperature": sensor temperature in degrees C (number), "coolerOn": boolean, "setpoint": cooler set-point temperature (number, degrees C), "power": cooler power (number, percent)  | returns a JSONRPC error if the camera does not have a cooler |
|`get_current_equipment`|none      |example: ` {"camera":{"name":"Simulator","connected":true},"mount":{"name":"On Camera","connected":true},"aux_mount":{"name":"Simulator","connected":true},"AO":{"name":"AO-Simulator","connected":false},"rotator":{"name":"Rotator Simulator .NET (ASCOM)","connected":false}} `|get the devices selected in the current equipment profile|
|`get_dec_guide_mode`|none      |string: "Off"/"Auto"/"North"/"South"|               |
|`get_exposure`|none      |integer: exposure time in milliseconds|               |
|`get_exposure_durations`|none      |array of integers: the list of valid exposure times in milliseconds |               |
|`get_guide_output_enabled`|none      |boolean: true when guide output is enabled|               |
|`get_lock_position`|none      |array: `[x, y]` coordinates of lock position, or `null` if lock position is not set |               |
|`get_lock_shift_enabled`|none      |boolean: true if lock shift enabled|               |
|`get_lock_shift_params`|none      |example: ` {"enabled":true,"rate":[1.10,4.50],"units":"arcsec/hr","axes":"RA/Dec"} `|               |
|`get_paused`|none      |boolean: true if paused|               |
|`get_pixel_scale`|none      |number: guider image scale in arc-sec/pixel. |               |
|`get_profile`|none      |` {"id":profile_id,"name":"profile_name"} ` |
|`get_profiles`|none      |array of ` {"id":profile_id,"name":"profile_name"} ` |  |
|`get_search_region`|none      |integer: search region radius |  |
|`get_ccd_temperature`|none      |"temperature": sensor temperature in degrees C (number) |  |
|`get_star_image`|integer: size (optional)      |frame: the frame number, width: the width of the image (pixels), height: height of the image (pixels), star\_pos: the star centroid position within the image, pixels: the image data, 16 bits per pixel, row-major order, base64 encoded | Returns an error if a star is not currently selected; The size parameter, if given, must be >= 15.The actual image size returned may be smaller than the requested image size (but will never be larger). The default image size is 15 pixels. |
|`get_use_subframes`|none |boolean:subframes_in_use|  |
|`get_variable_delay_settings`|none    |` {"Enabled":boolean, "ShortDelaySeconds":integer, "LongDelaySeconds":integer} ` |
|`guide`   | `settle`: object;<br>`recalibrate`: boolean, optional, default = false;<br>`roi`: array [x,y,width,height], optional, default = full frame | integer (0) | See below     |
|`guide_pulse`| integer: amount (pulse duration in milliseconds, or ao step count), string: direction ("N"/"S"/"E"/"W"/"Up"/"Down"/"Left"/"Right"), string (optional): which ("AO" or "Mount" [default]) | integer (0) | Returns an error if PHD2 is currently calibrating or guiding
|`loop`    |none      |integer (0)|start capturing, or, if guiding, stop guiding but continue capturing|
|`save_image`|none      |` {"filename":"full_path_to_FITS_image_file"} `| save the current image. The client should remove the file when done with it. |
|`set_algo_param`|string: axis, string: name, float: value    | integer(0)  | set a guide algorithm parameter on an axis |
|`set_connected`|boolean: connect| integer (0) | connect or disconnect all equipment |
|`set_dec_guide_mode`|string: mode ("Off"/"Auto"/"North"/"South") | integer(0) |               |
|`set_exposure`|integer: exposure time in milliseconds| integer (0)|
|`set_guide_output_enabled`|enabled: boolean      | integer(0) | Enables or disables guide output |
|`set_lock_position`|X: float, Y: float, EXACT: boolean (optional, default = true) | integer (0) | When EXACT is `true`, the lock position is moved to the exact given coordinates. When `false`, the current position is moved to the given coordinates and if a guide star is in range, the lock position is set to the coordinates of the guide star. |
|`set_lock_shift_enabled`|boolean: enable lock shift|integer (0)|               |
|`set_lock_shift_params`|` {"rate":[XRATE,YRATE],"units":UNITS,"axes":AXES} `|integer (0)| UNITS = "arcsec/hr" or "pixels/hr"; AXES = "RA/Dec" or "X/Y"|
|`set_paused`|PAUSED: boolean, FULL: string (optional) |integer (0)| When setting paused to `true`, an optional second parameter with value `"full"` can be provided to fully pause phd, including pausing looping exposures. Otherwise, exposures continue to loop, and only guide output is paused. Example: `{"method":"set_paused","params":[true,"full"],"id":42}` |
|`set_profile`|integer: profile id| integer (0) | Select an equipment profile. All equipment must be disconnected before switching profiles. |
|`set_variable_delay_settings`|`  {"Enabled":boolean, "ShortDelaySeconds":integer, "LongDelaySeconds":integer} ` | integer(0) |
|`shutdown`| | integer (0) | Close PHD2 |
|`stop_capture`|none      |integer (0)|Stop capturing (and stop guiding)|

### Settle parameter ###

The `SETTLE` parameter is used by the `guide` and `dither` commands to specify when PHD2 should consider guiding to be stable enough for imaging. `SETTLE` is an object with the following attributes:
  * `pixels` - maximum guide distance for guiding to be considered stable or "in-range"
  * `time` - minimum time to be in-range before considering guiding to be stable
  * `timeout` - time limit before settling is considered to have failed

So, for example, to request settling at less than 1.5 pixels for at least 10 seconds, with a time limit of 60 seconds, the settle object parameter would be:
```
{"pixels": 1.5, "time": 10, "timeout": 60}
```

### Guide Method ###

The `guide` method allows a client to request PHD2 to do whatever it needs to start guiding and to report when guiding is settled and stable.

When the `guide` method command is received, PHD2 will respond immediately indicating that the `guide` sequence has started. The `guide` method will return an error status if equipment is not connected. PHD will then:

  * start capturing if necessary
  * auto-select a guide star if one is not already selected. It the optional ROI parameter is present, star selection will be confined to the specified region of interest.
  * calibrate if necessary, or if the `recalibrate` parameter is true
  * wait for calibration to complete
  * start guiding if necessary
  * wait for settle (or timeout)
  * report progress of settling for each exposure (send `Settling` events)
  * report success or failure by sending a `SettleDone` event.

If the `guide` command is accepted, PHD is guaranteed to send a `SettleDone` event some time later indicating the success or failure of the guide sequence.  Note: if PHD2 is already guiding, the 'guide' RPC will only trigger another settling period - it will not stop and restart guiding.  To "start fresh" with guiding, first transmit the 'stop_capture' RPC, then transmit the 'guide' RPC.

Example
```
{"method": "guide", "params": {"settle": {"pixels": 1.5, "time": 8, "timeout": 40}}, "id": 42}
```

Example showing optional `recalibrate` and `roi` params
```
{"method": "guide", "params": {"settle": {"pixels": 1.5, "time": 8, "timeout": 40}, "recalibrate": false, "roi": [20, -50, 1280, 960]}, "id": 42}
```

### Dither Method ###

The `dither` method allows the client to request a random shift of the lock position by +/- PIXELS on each of the RA and Dec axes. If the RA\_ONLY parameter is true, or if the Dither RA Only option is set in the Brain, the dither will only be on the RA axis. The PIXELS parameter is multiplied by the Dither Scale value in the Brain.

Like the `guide` method, the `dither` method takes a `SETTLE` object parameter. PHD will send `Settling` and `SettleDone` events to indicate when guiding has stabilized after the dither.

Example
```
{"method": "dither", "params": {"amount": 10, "raOnly": false, "settle": {"pixels": 1.5, "time": 8, "timeout": 40}}, "id": 42}
```

# Application Notes #

1. Any of the PHD2 RPC commands that include a settling parameter can trigger long-running
operations.  For example, a 'guide' command may result in an extended sequence of looping,
auto-selection, calibration, start of guiding, and settling.  If the application sends another RPC that
can affect these state transitions before the first operation is completed, it may get an error return
message about "re-entrancy".  For example, issuing a command like 'dither' while a previous
settling event is still ongoing will result in an error return, and the dither will not be done.  It is the
responsibility of the application to monitor the PHD2 events and state-changes after starting one
of these long-running operations.  The best practice is to wait until the 'SettleDone' message has
been received, at which point the long-running operation has completed.

2. Although the available RPC commands permit a fine-grained control over PHD2 operations, it is
generally a poor practice to "micro-manage" the application.  For example, forcing calibrations or
flipping calibration data is normally unnecessary and adds confusion for the end-user.  These
things should be done only in special circumstances, not as a lazy "lowest-common-denominator"
approach.  Unnecessary calibrations, in particular, can be burdensome to end-users because
lower-end mounts often need to have Dec backlash cleared manually in order for the calibration
to get an accurate result.
