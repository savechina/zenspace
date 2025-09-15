package __package__.common.domain;

import lombok.Data;

import java.util.List;

/**
 * Result
 *
 * @author weirenyan
 * @date 2024/5/8
 */
@Data
public class Result<T> {
    private String code;
    private Boolean success;

    private String message;

    private T data;

    public static <T> Result<T> success(T data) {
        Result<T> result = new Result<>();

        result.data = data;
        result.success = true;
        result.code = "0";
        return result;
    }

    public static <T> Result<T> ok(T data) {
        Result<T> result = new Result<>();

        result.data = data;
        result.success = true;
        result.code = "0";
        return result;
    }

    public static <T> Result<T> failed(T data,String errorCode,String errorMessage) {
        Result<T> result = new Result<>();

        result.data = data;
        result.success = true;
        result.code = errorCode;
        result.message= errorMessage;
        return result;
    }
}
