package __package__.common.enums;

/**
 * 物料类型
 *
 */
public enum MaterialTypeEnum {

    /**
     * 商品物料
     */
    PRODUCT(1, "商品"),
    /**
     * 广告物料
     */
    AD(2, "广告"),
    ;

    public final int code;

    public final String desc;

    MaterialTypeEnum(int code, String desc) {
        this.code = code;
        this.desc = desc;
    }
}
