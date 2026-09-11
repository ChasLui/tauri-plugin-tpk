## Default Permission

What an ordinary frontend needs: poll for updates, download them, acknowledge that the running revision works, and read the current state. Notably absent is `reset`, which can clear the blacklist.

#### This default permission set includes the following:

- `allow-check`
- `allow-download`
- `allow-notify-ready`
- `allow-status`

## Permission Table

<table>
<tr>
<th>Identifier</th>
<th>Description</th>
</tr>


<tr>
<td>

`tpk:allow-mods`

</td>
<td>

Lets the frontend enable or disable mod layers. Nothing loads mod layers today and the command refuses; the set exists so the capability name is reserved rather than being invented later with different semantics. Desktop only — see `spec/tpk-v1.md` appendix A.4.


</td>
</tr>

<tr>
<td>

`tpk:allow-reset`

</td>
<td>

Lets the frontend discard downloaded content, optionally including the blacklist. A support path: clearing the blacklist re-enables installing a release that was previously found to be broken, so grant it deliberately.


</td>
</tr>

<tr>
<td>

`tpk:allow-check`

</td>
<td>

Enables the check command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`tpk:deny-check`

</td>
<td>

Denies the check command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`tpk:allow-download`

</td>
<td>

Enables the download command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`tpk:deny-download`

</td>
<td>

Denies the download command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`tpk:allow-notify-ready`

</td>
<td>

Enables the notify_ready command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`tpk:deny-notify-ready`

</td>
<td>

Denies the notify_ready command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`tpk:allow-reset`

</td>
<td>

Enables the reset command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`tpk:deny-reset`

</td>
<td>

Denies the reset command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`tpk:allow-set-mod-enabled`

</td>
<td>

Enables the set_mod_enabled command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`tpk:deny-set-mod-enabled`

</td>
<td>

Denies the set_mod_enabled command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`tpk:allow-status`

</td>
<td>

Enables the status command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`tpk:deny-status`

</td>
<td>

Denies the status command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`tpk:deny-all`

</td>
<td>

Blocks every tpk command for a window.

</td>
</tr>
</table>
